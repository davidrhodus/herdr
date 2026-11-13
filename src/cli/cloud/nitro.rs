//! One owned guest per dedicated EC2 parent. No access to unrelated parents.
use super::{bootstrap, model::*, transport::*};
use std::{
    io,
    process::Command,
    time::{Duration, Instant},
};

fn ssh(job: &Job, proof: &bootstrap::Proof, script: &[u8]) -> io::Result<Vec<u8>> {
    let target = bootstrap::trust(job, proof)?;
    let dir = job_dir(&job.id)?;
    let mut c = Command::new("ssh");
    c.args([
        "-F",
        "none",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "ControlPath=none",
        "-o",
    ])
    .arg(format!(
        "UserKnownHostsFile=\"{}\"",
        dir.join(format!("known_hosts-{}", proof.ip)).display()
    ))
    .arg("-i")
    .arg(dir.join("id_ed25519"))
    .arg("-T")
    .arg(target)
    .arg("sudo -n bash -s");
    capture(c, script, Duration::from_secs(1800))
}
pub(super) fn prepare(job: &mut Job, parent: bootstrap::Proof) -> io::Result<()> {
    let path = job_dir(&job.id)?.join("guest-bootstrap.sh");
    let guest_id = format!("{}-guest", job.id);
    if !path.exists() {
        let auth = bootstrap::enrollment(&settings()?)?;
        let public = bootstrap::identity(&job.id)?;
        let host = format!("herdr-guest-{}", &job.id[..12]);
        write(
            &path,
            bootstrap::script(&guest_id, &public, &auth, &host)?.as_bytes(),
        )?;
    }
    crate::platform::validate_private_key_path(&path, false)?;
    use base64::Engine;
    let bootstrap = base64::engine::general_purpose::STANDARD.encode(std::fs::read(path)?);
    let script = include_str!("nitro.sh")
        .replace("@JOB@", &job.id)
        .replace("@SHORT@", &job.id[..8])
        .replace("@BOOTSTRAP@", &bootstrap);
    job.phase("Preparing dedicated Nitro parent and persistent guest")?;
    let start = Instant::now();
    let mut rebooting = false;
    loop {
        let output = match ssh(job, &parent, script.as_bytes()) {
            Ok(output) => output,
            Err(_) if rebooting && start.elapsed() < Duration::from_secs(600) => {
                std::thread::sleep(Duration::from_secs(10));
                continue;
            }
            Err(e) => return Err(e),
        };
        if !String::from_utf8_lossy(&output).contains("HERDR-NITRO-REBOOT") {
            break;
        }
        job.phase("New Nitro parent is rebooting to reserve its memory pool")?;
        rebooting = true;
        if start.elapsed() > Duration::from_secs(600) {
            return Err(error(
                "Nitro memory pool is not ready; Resume after checking the dedicated parent",
            ));
        }
        std::thread::sleep(Duration::from_secs(30));
        // Retry only this verified parent. Bootstrap may take several minutes.
        let _ = bootstrap::wait_proof(job)?;
    }
    job.phase("Waiting for the Nitro guest to enroll in Tailscale")?;
    let proof = bootstrap::wait_proof_for(job, &guest_id)?;
    bootstrap::connect(job, proof)
}
pub(super) fn stop(job: &Job) -> io::Result<()> {
    let value = super::provider::inspect(job)?;
    if value["State"]["Name"] != "running" {
        return Ok(());
    }
    let console = super::provider::console(job)?;
    let Some(proof) = bootstrap::proof(&console, &job.id)? else {
        return Ok(());
    };
    let script=format!("set -eu\ntest \"$(cat /var/lib/herdr-nitro/owner)\" = '{}'\nsystemctl stop herdr-nitro-{}.service\n",job.id,&job.id[..8]);
    ssh(job, &proof, script.as_bytes())?;
    Ok(())
}
pub(super) fn repair_guest(job: &Job) -> io::Result<()> {
    let parent = bootstrap::wait_proof(job)?;
    use base64::Engine;
    let payload = base64::engine::general_purpose::STANDARD
        .encode(std::fs::read(job_dir(&job.id)?.join("guest-bootstrap.sh"))?);
    let script = repair_script(&job.id, &payload);
    ssh(job, &parent, script.as_bytes())?;
    Ok(())
}

fn repair_script(id: &str, payload: &str) -> String {
    format!(
        r#"set -eu
base=/var/lib/herdr-nitro
test -e "$base/owner" || exit 0
test "$(cat "$base/owner")" = '{}'
exec 8>"$base/lock"
flock -n 8
load=$(systemctl show -p LoadState --value herdr-nitro-{}.service)
if test "$load" != not-found; then
  systemctl stop herdr-nitro-@UNIT@.service
fi
exec 9>"$base/disk.lock"
flock -n 9
if test -f "$base/image-ready"; then
  mount -o loop "$base/root.ext4" "$base/mnt"
  trap 'umount "$base/mnt" || true' EXIT
  printf '%s' '{}' | base64 -d > "$base/mnt/root/herdr-first-boot"
  chmod 700 "$base/mnt/root/herdr-first-boot"
  install -d -m 700 "$base/mnt/var/lib/herdr-cloud"
  printf '%s' '{}' | base64 -d > "$base/mnt/var/lib/herdr-cloud/bootstrap"
  chmod 700 "$base/mnt/var/lib/herdr-cloud/bootstrap"
  umount "$base/mnt"
  trap - EXIT
fi
"#,
        id,
        &id[..8],
        payload,
        payload
    )
    .replace("@UNIT@", &id[..8])
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn repair_never_mounts_after_a_failed_stop_or_busy_disk_and_missing_owner_is_a_noop() {
        let dir = crate::platform::create_remote_ssh_config_dir("nitro-repair-test").unwrap();
        let owner = "12345678-1234-4234-8234-123456789abc";
        let script = super::repair_script(owner, "dGVzdA==").replace(
            "base=/var/lib/herdr-nitro",
            &format!(
                "base={}",
                crate::remote::shell_quote(&dir.to_string_lossy())
            ),
        );
        let marker = dir.join("mounted");
        let mount = format!(
            "mount() {{ touch {}; }}\n",
            crate::remote::shell_quote(&marker.to_string_lossy())
        );
        let run = |prefix: &str| {
            let mut command = std::process::Command::new("bash");
            command.arg("-s");
            super::capture(
                command,
                format!("{prefix}{mount}{script}").as_bytes(),
                std::time::Duration::from_secs(5),
            )
        };
        let failed="systemctl() { if test \"$1\" = show; then echo loaded; else return 1; fi; }\nflock() { return 0; }\n";
        assert!(run(failed).is_ok()); // No child state, no stop or mount.
        std::fs::write(dir.join("owner"), owner).unwrap();
        std::fs::write(dir.join("image-ready"), "").unwrap();
        assert!(run(failed).is_err());
        assert!(!marker.exists());
        let busy = "systemctl() { echo loaded; }\nflock() { return 1; }\n";
        assert!(run(busy).is_err());
        assert!(!marker.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn guest_lifecycle_has_persistent_disk_owned_network_and_metadata_block() {
        let s = include_str!("nitro.sh");
        for expected in [
            "root.ext4",
            "169.254.169.254 reject",
            "169.254.169.253",
            "herdr_nitro_@SHORT@",
            "/owner",
            "mem_pool/free",
            "pcie_slots_in_use",
            "--config-file",
            "direct_mode",
            "systemctl enable --now",
        ] {
            assert!(s.contains(expected), "{expected}");
        }
        assert!(!s.contains("rm -rf"));
    }
}
