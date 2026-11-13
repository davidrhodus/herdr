#!/bin/bash
set -eu
umask 077
# HERDR-ENROLLMENT @AUTH@
# Install a durable retry service before package/network work. Closing the
# manager or a temporary repository outage must not abandon first boot.
if test "${HERDR_CLOUD_BOOTSTRAP:-}" != 1; then
    install -d -m 700 /var/lib/herdr-cloud
    install -m 700 "$0" /var/lib/herdr-cloud/bootstrap
    cat > /etc/systemd/system/herdr-cloud-bootstrap.service <<'EOF'
[Unit]
After=network-online.target
Wants=network-online.target
[Service]
Type=oneshot
Environment=HERDR_CLOUD_BOOTSTRAP=1
ExecStart=/bin/bash /var/lib/herdr-cloud/bootstrap
Restart=on-failure
RestartSec=30
RemainAfterExit=yes
[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable --now --no-block herdr-cloud-bootstrap
    exit 0
fi
if command -v apt-get >/dev/null; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y curl openssh-server sudo jq ca-certificates
else
    dnf install -y curl openssh-server sudo jq
fi
id herdr >/dev/null 2>&1 || useradd -m -s /bin/bash herdr
install -d -m 700 -o herdr -g herdr /home/herdr/.ssh
printf '%s\n' '@PUBLIC@' > /home/herdr/.ssh/authorized_keys
chown herdr:herdr /home/herdr/.ssh/authorized_keys
chmod 600 /home/herdr/.ssh/authorized_keys
printf 'herdr ALL=(ALL) NOPASSWD:ALL\n' > /etc/sudoers.d/herdr
chmod 440 /etc/sudoers.d/herdr
printf 'PasswordAuthentication no\nPermitRootLogin no\n' > /etc/ssh/sshd_config.d/90-herdr.conf
ssh-keygen -A
systemctl enable --now ssh 2>/dev/null || systemctl enable --now sshd
if ! command -v tailscale >/dev/null; then
    curl --proto '=https' --tlsv1.2 -fsSL https://tailscale.com/install.sh -o /root/herdr-tailscale-install.sh
    sh /root/herdr-tailscale-install.sh
    rm -f /root/herdr-tailscale-install.sh
fi
systemctl enable --now tailscaled
if ! tailscale status --json | jq -e '.BackendState == "Running"' >/dev/null; then
    auth='@AUTH@'
    metadata=/root/herdr-latest-bootstrap
    token=$(curl -fsS --connect-timeout 2 --max-time 3 -X PUT -H 'X-aws-ec2-metadata-token-ttl-seconds: 60' http://169.254.169.254/latest/api/token 2>/dev/null || true)
    if test -n "$token"; then
        curl -fsS --connect-timeout 2 --max-time 3 -H "X-aws-ec2-metadata-token: $token" http://169.254.169.254/latest/user-data > "$metadata" 2>/dev/null || true
    else
        curl -fsS --connect-timeout 2 --max-time 3 -H 'Metadata-Flavor: Google' http://metadata.google.internal/computeMetadata/v1/instance/attributes/startup-script > "$metadata" 2>/dev/null || true
    fi
    latest=$(sed -n 's/^# HERDR-ENROLLMENT \(tskey-auth-[A-Za-z0-9-]*\)$/\1/p' "$metadata")
    rm -f "$metadata"
    if test -n "$latest" && test "$(printf '%s\n' "$latest" | wc -l)" -eq 1; then auth=$latest; fi
    printf '%s' "$auth" > /root/herdr-enrollment-key
    tailscale up --auth-key=file:/root/herdr-enrollment-key --hostname='@HOSTNAME@'
    rm -f /root/herdr-enrollment-key
fi
cat > /usr/local/sbin/herdr-cloud-report <<'EOF'
#!/bin/bash
set -eu
node=$(tailscale status --json | jq -er '.Self.ID')
ip=$(tailscale ip -4)
pub=$(awk '{print $1 " " $2}' /etc/ssh/ssh_host_ed25519_key.pub)
printf 'HERDR-READY @JOB@ %s %s %s\n' "$node" "$ip" "$pub" > /dev/console
EOF
chmod 700 /usr/local/sbin/herdr-cloud-report
cat > /etc/systemd/system/herdr-cloud-report.service <<'EOF'
[Unit]
After=tailscaled.service network-online.target
Wants=network-online.target tailscaled.service
[Service]
Type=oneshot
ExecStart=/usr/local/sbin/herdr-cloud-report
Restart=on-failure
RestartSec=10
[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now herdr-cloud-report
