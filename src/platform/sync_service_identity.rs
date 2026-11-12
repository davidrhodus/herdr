//! Service namespaces follow state/config identity, independent of UI sessions.
use std::path::Path;
pub(super) fn namespace(config: &Path, state: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for path in [config, state] {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let value = path.to_string_lossy();
        hash.update(value.len().to_le_bytes());
        hash.update(value.as_bytes());
    }
    format!("{:x}", hash.finalize())[..16].into()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinct_state_namespaces_do_not_replace_each_others_services() {
        let config = Path::new("/test/config/herdr");
        assert_ne!(
            namespace(config, Path::new("/test/desktop/herdr")),
            namespace(config, Path::new("/test/laptop/herdr"))
        );
    }
}
