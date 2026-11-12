//! An isolated destination config prevents additive IdentityFile fallback.
use crate::client::endpoint::catalog_sync::restricted::MARKER;
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

struct RouteFile(PathBuf);
impl Drop for RouteFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn config_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn minimal_config(resolved: &str) -> io::Result<String> {
    let values: std::collections::BTreeMap<_, _> = resolved
        .lines()
        .filter_map(|line| line.split_once(' '))
        .collect();
    let hostname = values
        .get("hostname")
        .ok_or_else(|| io::Error::other("SSH routing has no hostname"))?;
    let user = values
        .get("user")
        .ok_or_else(|| io::Error::other("SSH routing has no username"))?;
    let port = values
        .get("port")
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p != 0)
        .ok_or_else(|| io::Error::other("SSH routing has no valid port"))?;
    if hostname.is_empty()
        || user.is_empty()
        || !hostname
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-_:[]%".contains(c))
        || !user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-+@\\".contains(c))
    {
        return Err(io::Error::other(
            "Unsafe hostname or username in resolved SSH configuration",
        ));
    }
    let mut config = format!(
        "Host *\n  HostName {}\n  User {}\n  Port {port}\n",
        config_quote(hostname),
        config_quote(user)
    );
    for key in [
        "hostkeyalias",
        "userknownhostsfile",
        "globalknownhostsfile",
        "checkhostip",
        "hostkeyalgorithms",
        "kexalgorithms",
        "ciphers",
        "macs",
        "addressfamily",
        "bindaddress",
        "bindinterface",
        "proxyusefdpass",
        "proxycommand",
    ] {
        if let Some(value) = values.get(key).filter(|v| **v != "none") {
            if value.len() > 16 * 1024 || value.chars().any(char::is_control) {
                return Err(io::Error::other("Invalid resolved SSH routing option"));
            }
            config.push_str(&format!("  {key} {value}\n"));
        }
    }
    if values
        .get("proxycommand")
        .is_none_or(|value| *value == "none")
    {
        if let Some(jumps) = values.get("proxyjump").filter(|value| **value != "none") {
            let hops: Vec<_> = jumps.split(',').collect();
            if hops.is_empty()
                || hops.len() > 16
                || jumps.len() > 4096
                || jumps.chars().any(|c| c.is_control() || c.is_whitespace())
            {
                return Err(io::Error::other("Invalid SSH jump route"));
            }
            let last = hops
                .last()
                .ok_or_else(|| io::Error::other("Missing SSH jump host"))?;
            let destination = if last.starts_with("ssh://") {
                last.to_string()
            } else {
                format!("ssh://{last}")
            };
            let mut args = vec![
                "ssh".to_owned(),
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "StrictHostKeyChecking=yes".into(),
                "-o".into(),
                "ForwardAgent=no".into(),
                "-W".into(),
                "[%h]:%p".into(),
            ];
            if hops.len() > 1 {
                args.extend(["-J".into(), hops[..hops.len() - 1].join(",")]);
            }
            args.push(destination);
            // Hop authentication uses the user's existing route; destination
            // authentication stays isolated in this dedicated configuration.
            config.push_str(&format!(
                "  ProxyCommand {}\n",
                args.iter()
                    .map(|arg| super::super::shell_quote(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
    }
    Ok(config)
}

fn command(target: &str, config: &Path, private: &Path, request: &str) -> Command {
    let mut command = Command::new("ssh");
    command
        .arg("-F")
        .arg(config)
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "UpdateHostKeys=no",
            "-o",
            "ControlPath=none",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPersist=no",
            "-o",
            "IdentityAgent=none",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "PreferredAuthentications=publickey",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "GSSAPIAuthentication=no",
            "-o",
            "HostbasedAuthentication=no",
            "-o",
            "PubkeyAcceptedAlgorithms=ssh-ed25519",
            "-o",
            "CertificateFile=none",
            "-o",
            "AddKeysToAgent=no",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ForwardX11=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ConnectionAttempts=1",
            "-o",
            "NumberOfPasswordPrompts=0",
            "-i",
        ])
        .arg(private)
        .arg("-T")
        .arg(target)
        .arg(request)
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env_remove("SSH_ASKPASS");
    command
}
pub(super) fn request(
    target: &str,
    directory: &Path,
    request: &str,
    input: &[u8],
) -> io::Result<Output> {
    let resolved = super::super::attach::sync_ssh_configuration(target)?;
    let contents = minimal_config(&resolved)?;
    let route = RouteFile(directory.join(format!(
        "route-{}.conf",
        crate::client::endpoint::ProfileId::generate()
    )));
    let mut file = crate::platform::create_private_state_file(&route.0)?;
    use std::io::Write;
    file.write_all(contents.as_bytes())?;
    drop(file);
    let child = command(target, &route.0, &directory.join("id_ed25519"), request)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    super::super::process::wait_with_input_output_limit(
        child,
        input.to_vec(),
        Duration::from_secs(20),
        Arc::new(AtomicBool::new(false)),
        herdr_sync::MAX_BODY + 64 * 1024,
    )
}
pub(super) fn json_request(
    target: &str,
    directory: &Path,
    verb: &str,
    input: &[u8],
) -> io::Result<serde_json::Value> {
    let output = request(target, directory, verb, input)?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "Restricted catalog SSH request failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(2048)
                .collect::<String>()
        )));
    }
    let marker = format!("{MARKER}\n");
    let consumed = output
        .stdout
        .windows(marker.len())
        .position(|window| window == marker.as_bytes())
        .ok_or_else(|| io::Error::other("Missing restricted catalog response marker"))?
        + marker.len();
    let body = &output.stdout[consumed..];
    if body.len() > herdr_sync::MAX_BODY {
        return Err(io::Error::other(
            "Restricted catalog response exceeds size limit",
        ));
    }
    serde_json::from_slice(body).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destination_config_excludes_regular_keys_masters_and_other_authentication() {
        let config = minimal_config("hostname server.example\nuser david\nport 22\nidentityfile /regular/admin-key\nidentityagent /regular/agent\ncontrolpath /regular/master\nlocalcommand touch /tmp/unwanted\nremoteforward 1234\nuserknownhostsfile /home/d/.ssh/known_hosts /home/d/.ssh/known_hosts2\n").unwrap();
        for forbidden in [
            "admin-key",
            "identityagent",
            "controlpath",
            "localcommand",
            "remoteforward",
            "Include",
        ] {
            assert!(!config.contains(forbidden));
        }
        let command = command(
            "server",
            Path::new("/isolated/config"),
            Path::new("/only-sync-key"),
            "fixed-request",
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        for required in [
            "/only-sync-key",
            "ControlPath=none",
            "IdentityAgent=none",
            "IdentitiesOnly=yes",
            "PasswordAuthentication=no",
            "PreferredAuthentications=publickey",
            "ClearAllForwardings=yes",
            "PubkeyAcceptedAlgorithms=ssh-ed25519",
        ] {
            assert!(args.iter().any(|arg| arg == required));
        }
        assert!(!args.iter().any(|arg| arg == "/regular/admin-key"));
    }
    #[test]
    fn jump_route_does_not_reinclude_destination_identity_settings() {
        let config = minimal_config("hostname server.example\nuser david\nport 2222\nproxyjump first,david@last:2200\nidentityfile /admin\n").unwrap();
        assert!(config.contains("ProxyCommand ssh"));
        assert!(config.contains("ssh://david@last:2200"));
        assert!(config.contains("-J first"));
        assert!(!config.contains("/admin"));
        assert!(!config.contains("Include"));
    }
}
