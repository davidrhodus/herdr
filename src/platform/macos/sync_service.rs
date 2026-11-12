//! Persistent per-user launch agent, separate from every pane/server job.
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};
fn label(config: &Path, state: &Path) -> String {
    format!(
        "dev.herdr.catalog-sync.{}",
        super::sync_service_identity::namespace(config, state)
    )
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn spec(executable: &Path, config: &Path, state: &Path, version: &str) -> String {
    let label = label(config, state);
    let exe = xml(&executable.to_string_lossy());
    let config = xml(&config.parent().unwrap_or(config).to_string_lossy());
    let state_home = xml(&state.parent().unwrap_or(state).to_string_lossy());
    let log = xml(&state.join("client/sync-service.log").to_string_lossy());
    let version = xml(version);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{exe}</string><string>sync</string><string>serve</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>30</integer>
<key>ProcessType</key><string>Background</string><key>LowPriorityIO</key><true/>
<key>EnvironmentVariables</key><dict>
<key>XDG_CONFIG_HOME</key><string>{config}</string><key>XDG_STATE_HOME</key><string>{state_home}</string>
<key>HERDR_SYNC_SERVICE</key><string>1</string><key>HERDR_SYNC_BUILD</key><string>{version}</string>
<key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin</string>
</dict><key>StandardOutPath</key><string>{log}</string><key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#
    )
}
fn domain() -> String {
    // SAFETY: getuid has no preconditions.
    format!("gui/{}", unsafe { libc::getuid() })
}
fn plist_path(config: &Path, state: &Path) -> io::Result<PathBuf> {
    let label = label(config, state);
    Ok(PathBuf::from(
        std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is unavailable"))?,
    )
    .join(format!("Library/LaunchAgents/{label}.plist")))
}
fn launch(args: &[&str]) -> io::Result<()> {
    let result = Command::new("/bin/launchctl").args(args).output()?;
    if result.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Background sync service: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )))
    }
}
pub(crate) fn install(
    executable: &Path,
    config: &Path,
    state: &Path,
    version: &str,
) -> io::Result<()> {
    let path = plist_path(config, state)?;
    let contents = spec(executable, config, state, version);
    let target = format!("{}/{}", domain(), label(config, state));
    if std::fs::read(&path).ok().as_deref() == Some(contents.as_bytes())
        && launch(&["print", &target]).is_ok()
    {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().expect("plist parent"))?;
    std::fs::create_dir_all(state.join("client"))?;
    let log = state.join("client/sync-service.log");
    if !log.exists() {
        let _file = crate::platform::create_private_state_file(&log)?;
    }
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = crate::platform::create_private_state_file(&temp)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, &path)?;
    let _ = launch(&["bootout", &target]);
    launch(&["bootstrap", &domain(), &path.to_string_lossy()])
}
pub(crate) fn stop() -> io::Result<()> {
    let config = crate::config::config_dir();
    let state = crate::config::state_dir();
    let label = label(&config, &state);
    let target = format!("{}/{label}", domain());
    if launch(&["print", &target]).is_ok() {
        launch(&["bootout", &target])?;
    }
    match std::fs::remove_file(plist_path(&config, &state)?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_agent_uses_exact_argv_and_private_namespace_without_shell() {
        let text = spec(
            Path::new("/Users/a&b/.local/bin/herdr"),
            Path::new("/private/config/herdr"),
            Path::new("/private/state/herdr"),
            "local.123",
        );
        assert!(text.contains("/Users/a&amp;b/.local/bin/herdr"));
        assert!(text.contains("<string>sync</string><string>serve</string>"));
        assert!(text.contains("<string>/private/state</string>"));
        assert!(!text.contains("/bin/sh"));
        assert!(text.contains("<key>RunAtLoad</key><true/>"));
    }
}
