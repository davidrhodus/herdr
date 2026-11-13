use std::{io, path::Path};
pub(crate) fn make_executable(_: &Path) -> io::Result<()> {
    Err(io::Error::other(
        "Private updates unsupported on this platform",
    ))
}
pub(crate) fn reexec(_: &Path) -> io::Result<()> {
    Err(io::Error::other(
        "Automatic client reconnect unsupported on this platform",
    ))
}

pub(crate) fn install_service(_: &Path, _: &str) -> io::Result<()> {
    Err(io::Error::other("Private update service unsupported"))
}
