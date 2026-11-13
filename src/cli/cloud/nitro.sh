#!/bin/bash
set -eu
umask 077
base=/var/lib/herdr-nitro
install -d -m 700 "$base"
modprobe nitro_enclaves
exec 9>"$base/lock"
flock -n 9
if test -e "$base/owner"; then
    test "$(cat "$base/owner")" = '@JOB@'
else
    # The parent is dedicated to this job. Never adopt another guest's host.
    test "$(cat /sys/class/misc/nitro_enclaves/pcie_slots_in_use)" = 0
    printf '%s' '@JOB@' > "$base/owner"
fi
test "$(printf '%s\n' '6.18.51-120.162.amzn2023' "$(uname -r)" | sort -V | head -n1)" = '6.18.51-120.162.amzn2023'
if ! grep -q 'nitro_enclaves.mempool=8G@0' /proc/cmdline; then
    grubby --update-kernel=ALL --args='nitro_enclaves.mempool=8G@0'
    echo HERDR-NITRO-REBOOT
    systemd-run --unit=herdr-nitro-reboot --on-active=3s /usr/bin/systemctl reboot >/dev/null
    exit 0
fi
if systemctl is-active --quiet herdr-nitro-@SHORT@.service; then
    echo HERDR-NITRO-RUNNING
    exit 0
fi
test "$(cat /sys/class/misc/nitro_enclaves/pcie_slots_in_use)" -lt "$(cat /sys/class/misc/nitro_enclaves/pcie_slots)"
# mem_pool/free is bytes; reserve overhead beyond the 2048 MiB guest.
test "$(cat /sys/class/misc/nitro_enclaves/mem_pool/free)" -ge $((2304 * 1024 * 1024))
# Reject an overlapping host/VPC route before creating the guest network.
# A dedicated parent's existing route to this range would become ambiguous.
ip -4 route show | awk '$1 == "default" {next} /dev hr@SHORT@/ {next} $1 ~ /^[0-9]+\./ {print $1}' | while read -r route; do
    prefix=${route#*/}
    network=${route%/*}
    test "$prefix" != "$route" || prefix=32
    test "$prefix" -le 30 || prefix=30
    IFS=. read -r a b c d <<< "$network"
    IFS=. read -r x y z w <<< 172.30.0.0
    if test "$prefix" -gt 0; then
        mask=$(( (0xffffffff << (32-prefix)) & 0xffffffff ))
        host=$(( (a<<24) | (b<<16) | (c<<8) | d ))
        guest=$(( (x<<24) | (y<<16) | (z<<8) | w ))
        test $((host & mask)) -ne $((guest & mask))
    fi
done
dnf install -y nftables dracut e2fsprogs curl tar jq
if ! test -x /usr/local/bin/herdr-firecracker-1.18.0; then
    curl --proto '=https' --tlsv1.2 -fsSL https://github.com/firecracker-microvm/firecracker/releases/download/v1.18.0/firecracker-v1.18.0-x86_64.tgz -o "$base/firecracker.tgz"
    tar -xzf "$base/firecracker.tgz" -C "$base"
    install -m 755 "$base/release-v1.18.0-x86_64/firecracker-v1.18.0-x86_64" /usr/local/bin/herdr-firecracker-1.18.0
fi
if ! test -e "$base/image-ready"; then
    # Build in a temporary image, then atomically publish it. Failed builds are
    # never adopted as persistent guest data on Resume.
    truncate -s 16G "$base/root.build"
    mkfs.ext4 -F "$base/root.build" >/dev/null
    install -d "$base/mnt"
    mount -o loop "$base/root.build" "$base/mnt"
    trap 'umount "$base/mnt" || true' EXIT
    dnf --installroot="$base/mnt" --releasever=2023 install -y dnf systemd systemd-networkd bash coreutils iproute openssh-server sudo jq curl ca-certificates kernel6.18
    cp -a "/lib/modules/$(uname -r)" "$base/mnt/lib/modules/"
    install -m 755 "$(command -v tailscale)" "$base/mnt/usr/bin/tailscale"
    install -m 755 "$(command -v tailscaled)" "$base/mnt/usr/bin/tailscaled"
    install -d "$base/mnt/etc/systemd/system" "$base/mnt/etc/systemd/network" "$base/mnt/root"
    printf '%s' '@BOOTSTRAP@' | base64 -d > "$base/mnt/root/herdr-first-boot"
    chmod 700 "$base/mnt/root/herdr-first-boot"
    cat > "$base/mnt/etc/systemd/system/tailscaled.service" <<'EOF'
[Unit]
After=network.target
[Service]
Type=notify
RuntimeDirectory=tailscale
StateDirectory=tailscale
ExecStart=/usr/bin/tailscaled --state=/var/lib/tailscale/tailscaled.state --socket=/run/tailscale/tailscaled.sock --port=41641
Restart=on-failure
[Install]
WantedBy=multi-user.target
EOF
    cat > "$base/mnt/etc/systemd/network/10-herdr.network" <<'EOF'
[Match]
Name=eth0
[Network]
Address=172.30.0.2/30
Gateway=172.30.0.1
DNS=169.254.169.253
EOF
    printf 'nameserver 169.254.169.253\n' > "$base/mnt/etc/resolv.conf"
    cat > "$base/mnt/etc/systemd/system/herdr-first-boot.service" <<'EOF'
[Unit]
After=network-online.target
Wants=network-online.target
[Service]
Type=oneshot
ExecStart=/bin/bash /root/herdr-first-boot
RemainAfterExit=yes
Restart=on-failure
RestartSec=15
StandardOutput=journal+console
StandardError=journal+console
[Install]
WantedBy=multi-user.target
EOF
    systemctl --root="$base/mnt" enable systemd-networkd herdr-first-boot
    cp "/boot/vmlinuz-$(uname -r)" "$base/vmlinuz"
    dracut --force --no-hostonly --add-drivers 'virtio_pci virtio_blk virtio_net ext4 tun' --kver "$(uname -r)" "$base/initrd.img"
    umount "$base/mnt"
    trap - EXIT
    mv "$base/root.build" "$base/root.ext4"
    touch "$base/image-ready"
fi
cat > "$base/network" <<'EOF'
#!/bin/bash
set -eu
ip link show hr@SHORT@ >/dev/null 2>&1 || ip tuntap add dev hr@SHORT@ mode tap
ip addr replace 172.30.0.1/30 dev hr@SHORT@
ip link set hr@SHORT@ up
sysctl -w net.ipv4.ip_forward=1 >/dev/null
iface=$(ip route show default | awk '{print $5; exit}')
nft list table ip herdr_nitro_@SHORT@ >/dev/null 2>&1 && nft delete table ip herdr_nitro_@SHORT@
nft -f - <<RULES
table ip herdr_nitro_@SHORT@ {
 chain forward {
  type filter hook forward priority -10; policy accept;
  iifname "hr@SHORT@" ip daddr 169.254.169.254 reject
 }
 chain postrouting {
  type nat hook postrouting priority srcnat; policy accept;
  iifname "hr@SHORT@" oifname "$iface" masquerade
 }
}
RULES
EOF
chmod 700 "$base/network"
cat > "$base/config.json" <<'EOF'
{"boot-source":{"kernel_image_path":"/var/lib/herdr-nitro/vmlinuz","initrd_path":"/var/lib/herdr-nitro/initrd.img","boot_args":"console=ttyS0 net.ifnames=0 root=/dev/vda rw"},"machine-config":{"vcpu_count":2,"mem_size_mib":2048},"drives":[{"drive_id":"root","path_on_host":"/var/lib/herdr-nitro/root.ext4","is_root_device":true,"is_read_only":false}],"network-interfaces":[{"iface_id":"eth0","host_dev_name":"hr@SHORT@"}],"vsock":{"guest_cid":3,"uds_path":"/run/herdr-nitro-@SHORT@/v.sock"},"enclave":{"cpu_overcommit":true,"direct_mode":true,"debug_mode":true}}
EOF
cat > /etc/systemd/system/herdr-nitro-@SHORT@.service <<'EOF'
[Unit]
After=network-online.target
Wants=network-online.target
[Service]
RuntimeDirectory=herdr-nitro-@SHORT@
ExecStartPre=/var/lib/herdr-nitro/network
ExecStartPre=/usr/bin/rm -f /run/herdr-nitro-@SHORT@/v.sock
ExecStart=/usr/bin/flock --no-fork /var/lib/herdr-nitro/disk.lock /usr/local/bin/herdr-firecracker-1.18.0 --no-api --config-file /var/lib/herdr-nitro/config.json
StandardOutput=journal+console
StandardError=journal+console
KillMode=control-group
TimeoutStopSec=30
[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable --now herdr-nitro-@SHORT@.service
echo HERDR-NITRO-RUNNING
