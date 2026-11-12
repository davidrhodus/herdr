use std::{io, path::Path};
pub(crate) const SUPPORTED: bool = false;
pub(crate) fn install(
    _exe: &Path,
    _root: &Path,
    _registry: &str,
    _public: &str,
    _fingerprint: &str,
) -> io::Result<()> {
    Err(io::Error::other(
        "Restricted sync-key enrollment requires a Unix SSH server",
    ))
}
