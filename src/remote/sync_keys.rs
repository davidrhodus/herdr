//! Catalog-only credentials, separate from terminal identities and SSH masters.
mod transport;
use crate::client::endpoint::catalog_sync::restricted::{EXCHANGE, STATUS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    version: u32,
    registry_id: String,
    fingerprint: String,
}

fn directory_path(target: &str) -> PathBuf {
    crate::config::state_dir()
        .join("client/sync-keys")
        .join(format!("{:x}", Sha256::digest(target.as_bytes())))
}
pub(crate) fn status(target: &str) -> serde_json::Value {
    match load(&directory_path(target)) {
        Ok(Some(binding)) => {
            serde_json::json!({"required":true,"enrolled":true,"fingerprint":binding.fingerprint})
        }
        Ok(None) => serde_json::json!({"required":true,"enrolled":false}),
        Err(error) => {
            serde_json::json!({"required":true,"enrolled":false,"error":error.to_string()})
        }
    }
}
fn directory(target: &str) -> io::Result<PathBuf> {
    crate::client::endpoint::SavedSshEndpoint::new("Sync host", target, "default")
        .map_err(io::Error::other)?;
    if !target
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-@:/[]%+~\\".contains(c))
    {
        return Err(io::Error::other("Invalid characters in sync SSH target"));
    }
    let root = crate::config::state_dir().join("client/sync-keys");
    fs::create_dir_all(
        root.parent()
            .ok_or_else(|| io::Error::other("Invalid sync key directory"))?,
    )?;
    super::key_setup::ensure_directory(&root)?;
    let directory = directory_path(target);
    super::key_setup::ensure_directory(&directory)?;
    Ok(directory)
}
fn load(directory: &Path) -> io::Result<Option<Binding>> {
    let path = directory.join("binding.json");
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    crate::platform::validate_private_key_path(&path, false)?;
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(io::Error::other(
            "Restricted sync-key binding exceeds size limit",
        ));
    }
    let binding: Binding = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if binding.version != 1
        || !herdr_sync::valid_id(&binding.registry_id)
        || !binding.fingerprint.starts_with("SHA256:")
        || binding.fingerprint.len() != 50
    {
        return Err(io::Error::other(
            "Invalid restricted sync-key binding; refusing unrestricted fallback",
        ));
    }
    Ok(Some(binding))
}
fn check_status(value: &serde_json::Value, binding: &Binding) -> io::Result<()> {
    if value["restricted_sync_v1"] != true
        || value["enabled"] != true
        || value["source"]["kind"] != "local"
        || value["registry_id"] != binding.registry_id
        || value["key_fingerprint"] != binding.fingerprint
    {
        return Err(io::Error::other(
            "Restricted sync-key fingerprint or shared registry does not match",
        ));
    }
    Ok(())
}
fn ensure(target: &str) -> io::Result<Option<(PathBuf, Binding)>> {
    let directory = directory(target)?;
    let lock_path = directory.join("enrollment.lock");
    let lock = match crate::platform::create_private_state_file(&lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            crate::platform::validate_private_key_path(&lock_path, false)?;
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(lock_path)?
        }
        Err(error) => return Err(error),
    };
    lock.lock()?;
    let private = directory.join("id_ed25519");
    if let Some(binding) = load(&directory)? {
        crate::platform::validate_private_key_path(&private, false)?;
        // A revoked/broken active key never causes administrator-key fallback.
        return Ok(Some((directory, binding)));
    }
    match fs::symlink_metadata(&private) {
        Ok(_) => crate::platform::validate_private_key_path(&private, false)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            super::key_setup::generate_key(&directory, &private)?
        }
        Err(error) => return Err(error),
    }
    let (public, fingerprint) = super::key_setup::public_key(&private)?;
    let Some(reply) = super::attach::enroll_catalog_key(target, &public)? else {
        return Ok(None);
    };
    let registry_id = reply["registry_id"]
        .as_str()
        .filter(|id| herdr_sync::valid_id(id))
        .ok_or_else(|| io::Error::other("Invalid restricted registry identity"))?
        .to_owned();
    let binding = Binding {
        version: 1,
        registry_id,
        fingerprint,
    };
    if reply["key_fingerprint"] != binding.fingerprint {
        return Err(io::Error::other(
            "Enrolled public-key fingerprint does not match",
        ));
    }
    let status = transport::json_request(target, &directory, STATUS, &[])?;
    check_status(&status, &binding)?;
    let denied = transport::request(target, &directory, "herdr-sync-denied-test", &[])?;
    if denied.status.success()
        || !String::from_utf8_lossy(&denied.stderr).contains("HERDR_SYNC_ONLY_DENIED")
    {
        return Err(io::Error::other(
            "Could not verify the sync key's forced-command restriction",
        ));
    }
    crate::client::endpoint::store_private_json(
        &directory.join("binding.json"),
        &serde_json::to_vec(&binding).map_err(io::Error::other)?,
        "restricted sync identity",
    )
    .map_err(io::Error::other)?;
    tracing::info!(target, fingerprint=%binding.fingerprint, "Restricted catalog-only SSH key enrolled");
    Ok(Some((directory, binding)))
}

pub(super) fn host_identity(target: &str) -> io::Result<Option<String>> {
    let Some((directory, binding)) = ensure(target)? else {
        return Ok(None);
    };
    check_status(
        &transport::json_request(target, &directory, STATUS, &[])?,
        &binding,
    )?;
    Ok(Some(binding.registry_id))
}
pub(super) fn exchange(
    target: &str,
    document: &herdr_sync::Document,
) -> io::Result<herdr_sync::Document> {
    document.validate().map_err(io::Error::other)?;
    let (directory, _binding) = ensure(target)?
        .ok_or_else(|| io::Error::other("This SSH account is not hosting a shared server list; run `herdr sync host` on it first"))?;
    let value = transport::json_request(
        target,
        &directory,
        EXCHANGE,
        &serde_json::to_vec(document).map_err(io::Error::other)?,
    )?;
    let document: herdr_sync::Document = serde_json::from_value(value).map_err(io::Error::other)?;
    document.validate().map_err(io::Error::other)?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_binding_is_not_treated_as_a_missing_key() {
        let path = std::env::temp_dir().join(format!(
            "herdr-sync-binding-{}",
            crate::client::endpoint::ProfileId::generate()
        ));
        fs::create_dir_all(&path).unwrap();
        let mut file =
            crate::platform::create_private_state_file(&path.join("binding.json")).unwrap();
        use std::io::Write;
        file.write_all(b"invalid").unwrap();
        drop(file);
        assert!(load(&path).is_err());
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn status_must_match_the_enrolled_key_and_registry() {
        let binding = Binding {
            version: 1,
            registry_id: "a".repeat(32),
            fingerprint: format!("SHA256:{}", "b".repeat(43)),
        };
        let good = serde_json::json!({"enabled":true,"source":{"kind":"local"},"restricted_sync_v1":true,"registry_id":binding.registry_id,"key_fingerprint":binding.fingerprint});
        assert!(check_status(&good, &binding).is_ok());
        for (field, value) in [
            ("key_fingerprint", serde_json::json!("other")),
            ("registry_id", serde_json::json!("c".repeat(32))),
            ("restricted_sync_v1", serde_json::json!(false)),
        ] {
            let mut wrong = good.clone();
            wrong[field] = value;
            assert!(check_status(&wrong, &binding).is_err());
        }
    }
}
