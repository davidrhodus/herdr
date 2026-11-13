//! Per-computer identity paths. This file is never part of the synced catalog.
pub(crate) const REUSE_EXISTING_ENV: &str = "HERDR_SETUP_REUSE_EXISTING";
pub(crate) const EXPECTED_PROFILE_ENV: &str = "HERDR_SETUP_EXPECTED_PROFILE";
use std::{collections::BTreeMap, io::Read, path::PathBuf};

fn host_keys_path(target: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    crate::config::state_dir()
        .join("client/cloud-host-keys")
        .join(format!("{:x}", Sha256::digest(target.as_bytes())))
}
pub(crate) fn set_host_keys(target: &str, source: &std::path::Path) -> Result<(), String> {
    let path = host_keys_path(target);
    let parent = path.parent().ok_or("Invalid host-key path")?;
    crate::platform::create_private_state_directory(parent).map_err(|e| e.to_string())?;
    crate::platform::validate_private_key_path(source, false).map_err(|e| e.to_string())?;
    let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
    if bytes.len() > 4096 {
        return Err("Host keys exceed limit".into());
    }
    if path.exists() && std::fs::read(&path).map_err(|e| e.to_string())? != bytes {
        return Err(
            "Pinned cloud host key changed; inspect the instance before replacing it".into(),
        );
    }
    crate::client::endpoint::store_private_json(&path, &bytes, "cloud SSH host keys")
}
pub(super) fn apply_host_keys(command: &mut std::process::Command, target: &str) -> bool {
    let path = host_keys_path(target);
    if path.is_file() {
        pinned_options(command, target, &path);
        return true;
    }
    false
}

fn pinned_options(command: &mut std::process::Command, target: &str, path: &std::path::Path) {
    command.args([
        "-F",
        "none",
        "-o",
        "ControlPath=none",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPersist=no",
    ]);
    // Command-line options precede user SSH configuration. This also fences
    // user aliases and global known-host entries from replacing a cloud pin.
    command
        .arg("-o")
        .arg(format!(
            "UserKnownHostsFile=\"{}\"",
            path.to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        ))
        .args([
            "-o",
            "GlobalKnownHostsFile=none",
            "-o",
            "StrictHostKeyChecking=yes",
        ]);
    if let Some((_, host)) = target.rsplit_once('@') {
        command.arg("-o").arg(format!("HostKeyAlias={host}"));
    }
}

fn path() -> PathBuf {
    crate::config::state_dir().join("client/ssh-identities.json")
}
fn load() -> Result<BTreeMap<String, String>, String> {
    let file = match std::fs::File::open(path()) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 256 * 1024 {
        return Err("Local SSH settings exceed size limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid local SSH settings".into())
}
pub(crate) const SETUP_TARGET: &str = "HERDR_SETUP_IDENTITY_TARGET";
pub(crate) const SETUP_PATH: &str = "HERDR_SETUP_IDENTITY_PATH";
pub(crate) fn setup_identity(target: &str) -> Option<PathBuf> {
    (std::env::var(SETUP_TARGET).ok().as_deref() == Some(target))
        .then(|| std::env::var_os(SETUP_PATH).map(PathBuf::from))
        .flatten()
}
pub(crate) fn get(target: &str) -> Option<PathBuf> {
    setup_identity(target).or_else(|| load().ok()?.get(target).map(PathBuf::from))
}
pub(crate) fn absolute(identity: &str) -> Result<PathBuf, String> {
    let path = if let Some(suffix) = identity.strip_prefix("~/") {
        crate::platform::remote_ssh_config_paths()
            .user_config
            .and_then(|p| p.parent()?.parent().map(|p| p.join(suffix)))
            .ok_or("Cannot resolve home directory")?
    } else {
        PathBuf::from(identity)
    };
    let path =
        std::fs::canonicalize(path).map_err(|_| "Identity file does not exist on this computer")?;
    if !path.is_file() {
        return Err("Identity path must name a file".into());
    }
    Ok(path)
}
pub(crate) fn set(target: &str, identity: &str) -> Result<(), String> {
    crate::client::endpoint::SavedSshEndpoint::new("Server", target, "default")?;
    if identity.len() > 4096 || identity.chars().any(char::is_control) {
        return Err("Invalid identity-file path".into());
    }
    let identity = if identity.is_empty() {
        PathBuf::new()
    } else {
        absolute(identity)?
    };
    let parent = path()
        .parent()
        .ok_or("Invalid local SSH settings path")?
        .to_owned();
    std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    let lock_path = parent.join("ssh-identities.lock");
    let lock = match crate::platform::create_private_state_file(&lock_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(&lock_path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("Local SSH settings lock must not be a symlink".into());
            }
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(lock_path)
                .map_err(|e| e.to_string())?
        }
        Err(e) => return Err(e.to_string()),
    };
    lock.lock().map_err(|e| e.to_string())?;
    let mut settings = load()?;
    if identity.as_os_str().is_empty() {
        settings.remove(target);
    } else {
        settings.insert(target.into(), identity.to_string_lossy().into_owned());
    }
    crate::client::endpoint::store_private_json(
        &path(),
        &serde_json::to_vec(&settings).map_err(|e| e.to_string())?,
        "local SSH identities",
    )
}

#[cfg(test)]
mod cloud_pin_tests {
    use super::*;
    #[test]
    fn cloud_pins_disable_user_config_and_all_connection_reuse() {
        let mut command = std::process::Command::new("ssh");
        pinned_options(
            &mut command,
            "herdr@100.64.1.2",
            std::path::Path::new("/tmp/path with spaces/known_hosts"),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for expected in [
            "none",
            "ControlPath=none",
            "ControlMaster=no",
            "ControlPersist=no",
            "StrictHostKeyChecking=yes",
            "GlobalKnownHostsFile=none",
            "HostKeyAlias=100.64.1.2",
        ] {
            assert!(args.iter().any(|a| a == expected), "{expected}");
        }
        assert!(!args.iter().any(|a| a == "-S"));
    }
    #[cfg(unix)]
    #[test]
    fn openssh_resolves_cloud_options_without_a_control_socket_or_user_aliases() {
        let mut command = std::process::Command::new("ssh");
        pinned_options(
            &mut command,
            "herdr@100.64.1.2",
            std::path::Path::new("/tmp/cloud test/known_hosts"),
        );
        let output = command.args(["-G", "herdr@100.64.1.2"]).output().unwrap();
        assert!(output.status.success());
        let config = String::from_utf8(output.stdout).unwrap();
        assert!(config.contains("controlmaster false"));
        assert!(!config
            .lines()
            .any(|l| l.starts_with("controlpath ") && l != "controlpath none"));
        assert!(config.contains("stricthostkeychecking true"));
        assert!(config.contains("/tmp/cloud test/known_hosts"));
    }
}
