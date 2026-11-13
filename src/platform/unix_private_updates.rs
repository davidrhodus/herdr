use std::{io, path::Path};
pub(crate) fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}
pub(crate) fn reexec(path: &Path) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    Err(std::process::Command::new(path)
        .args(std::env::args_os().skip(1))
        .exec())
}

#[cfg(target_os = "macos")]
pub(crate) fn install_service(executable: &Path, identity: &str) -> io::Result<()> {
    super::sync_service::install_updater(
        executable,
        &crate::config::config_dir(),
        &crate::config::state_dir(),
        identity,
    )
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn install_service(_: &Path, _: &str) -> io::Result<()> {
    Err(io::Error::other(
        "Private update service supports Mac managers",
    ))
}
