use std::{io, path::Path};
pub(crate) fn install(_: &Path, _: &Path, _: &Path, _: &str) -> io::Result<()> {
    Err(io::Error::other("Background login sync is supported on macOS and Linux with a user service manager. Sync remains available while Herdr is open."))
}
pub(crate) fn stop() -> io::Result<()> {
    Ok(())
}
