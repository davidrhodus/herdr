//! Per-user systemd service; installing it does not change login/linger policy.
use std::{
    io::{self, Write},
    path::Path,
    process::Command,
};
fn unit(config: &Path, state: &Path) -> String {
    format!(
        "herdr-catalog-sync-{}.service",
        super::sync_service_identity::namespace(config, state)
    )
}
fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('\n', "\\n")
    )
}
fn spec(exe: &Path, config: &Path, state: &Path, version: &str) -> String {
    format!("[Unit]\nDescription=Herdr server-list sync\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart=:{} sync serve\nRestart=always\nRestartSec=15\nEnvironment={}\nEnvironment={}\nEnvironment={}\nEnvironment=HERDR_SYNC_SERVICE=1\n\n[Install]\nWantedBy=default.target\n",quote(&exe.to_string_lossy()),quote(&format!("XDG_CONFIG_HOME={}",config.parent().unwrap_or(config).display())),quote(&format!("XDG_STATE_HOME={}",state.parent().unwrap_or(state).display())),quote(&format!("HERDR_SYNC_BUILD={version}")))
}
fn systemctl(args: &[&str]) -> io::Result<()> {
    let result = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()?;
    if result.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Background sync service: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )))
    }
}
pub(crate) fn install(exe: &Path, config: &Path, state: &Path, version: &str) -> io::Result<()> {
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is unavailable"))?;
    let directory = std::path::PathBuf::from(home).join(".config/systemd/user");
    std::fs::create_dir_all(&directory)?;
    let unit = unit(config, state);
    let path = directory.join(&unit);
    let contents = spec(exe, config, state, version);
    let changed = std::fs::read(&path).ok().as_deref() != Some(contents.as_bytes());
    if changed {
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let mut file = crate::platform::create_private_state_file(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(temp, path)?;
        systemctl(&["daemon-reload"])?;
    }
    systemctl(&["enable", "--now", &unit])?;
    if changed {
        systemctl(&["restart", &unit])?;
    }
    Ok(())
}
pub(crate) fn stop() -> io::Result<()> {
    let unit = unit(&crate::config::config_dir(), &crate::config::state_dir());
    systemctl(&["disable", "--now", &unit])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_quotes_paths_and_never_uses_shell_or_root() {
        let text = spec(
            Path::new("/home/a b/herdr%test"),
            Path::new("/cfg/herdr"),
            Path::new("/state/herdr"),
            "local",
        );
        assert!(text.contains("ExecStart=:\"/home/a b/herdr%%test\" sync serve"));
        assert!(!text.contains("/bin/sh"));
        assert!(text.contains("XDG_STATE_HOME=/state"));
    }
}
