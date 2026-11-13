//! Client-owned provisioning records. These never enter the shared catalog.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io,
    path::{Path, PathBuf},
};
#[cfg(test)]
thread_local! {pub(super) static TEST_ROOT:std::cell::RefCell<Option<PathBuf>>=const {std::cell::RefCell::new(None)};}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) enum Provider {
    Aws,
    Gcp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Account {
    pub name: String,
    pub provider: Provider,
    pub configuration: String,
    /// AWS account ID or the authenticated Google account. Resolved before save.
    pub principal: String,
    pub location: String,
    pub project: String,
    pub image: String,
    pub size: String,
    pub subnet: String,
    pub security_group: String,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    pub accounts: Vec<Account>,
    pub oauth_client: String,
    pub tailnet: String,
    pub tag: String,
    #[serde(default)]
    pub enrollment: Option<Enrollment>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) enum Enrollment {
    Browser,
    OAuth,
}
impl Settings {
    pub fn enrollment_mode(&self) -> Enrollment {
        self.enrollment.unwrap_or(if self.oauth_client.is_empty() {
            Enrollment::Browser
        } else {
            Enrollment::OAuth
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Job {
    pub id: String,
    pub account: Account,
    pub name: String,
    pub nitro: bool,
    /// Frozen provider request, including a single-use enrollment key.
    /// Stored in a private file, never printed or synced.
    pub request: Value,
    pub resource: Option<String>,
    pub operation: Option<String>,
    pub submitted: bool,
    #[serde(default)]
    pub first_submission: Option<u64>,
    pub phase: String,
    pub target: Option<String>,
    pub endpoint: Option<String>,
    pub deleted: bool,
    #[serde(default)]
    pub pending_action: Option<Lifecycle>,
    #[serde(default)]
    pub expected_tailnet: Option<String>,
    #[serde(default)]
    pub reauth_nonce: Option<String>,
    #[serde(default)]
    pub pending_enrollment_repair: Option<EnrollmentRepair>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct EnrollmentRepair {
    pub nonce: String,
    pub parent_script: String,
    pub guest_script: Option<String>,
    pub parent_updated: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Lifecycle {
    pub name: String,
    pub token: String,
    pub operation: Option<String>,
    #[serde(default)]
    pub cleanup_operation: Option<String>,
}
pub(super) fn error(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
pub(super) fn token(value: &str) -> io::Result<&str> {
    if value.is_empty()
        || value.len() > 253
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-/:@".contains(&b))
    {
        return Err(error(
            "Use a name or identifier without spaces, control characters, or shell syntax",
        ));
    }
    Ok(value)
}
pub(super) fn uuid() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| error("Random generator unavailable"))?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}
pub(super) fn now() -> io::Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| error("System clock is invalid"))
}
pub(super) fn root() -> io::Result<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_ROOT.with(|r| r.borrow().clone()) {
        return Ok(root);
    }
    let root = crate::config::state_dir().join("client/cloud");
    crate::platform::create_private_state_directory(&root)?;
    Ok(root)
}
pub(super) fn job_dir(id: &str) -> io::Result<PathBuf> {
    if id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(error("Invalid job ID"));
    }
    let path = root()?.join(id);
    crate::platform::create_private_state_directory(&path)?;
    Ok(path)
}
pub(super) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| error("Missing state directory"))?;
    let tmp = parent.join(format!(".{}.tmp", uuid()?));
    let result = (|| {
        let mut f = crate::platform::create_private_state_file(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        crate::platform::replace_file(&tmp, path)?;
        crate::platform::sync_parent_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}
pub(super) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    crate::platform::validate_private_key_path(path, false)?;
    if std::fs::metadata(path)?.len() > 4 * 1024 * 1024 {
        return Err(error("Cloud state exceeds limit"));
    }
    serde_json::from_slice(&std::fs::read(path)?).map_err(|_| error("Invalid cloud state"))
}
pub(super) fn save<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    write(
        path,
        &serde_json::to_vec_pretty(value).map_err(|_| error("Cannot encode cloud state"))?,
    )
}
pub(super) fn settings() -> io::Result<Settings> {
    let p = root()?.join("accounts.json");
    if !p.exists() {
        return Ok(Settings::default());
    }
    read(&p)
}
impl Job {
    pub fn save(&self) -> io::Result<()> {
        save(&job_dir(&self.id)?.join("job.json"), self)
    }
    pub fn phase(&mut self, phase: &str) -> io::Result<()> {
        self.phase = phase.into();
        self.save()?;
        println!("{phase}");
        Ok(())
    }
}
pub(super) fn jobs() -> io::Result<Vec<Job>> {
    let mut jobs = Vec::new();
    for entry in std::fs::read_dir(root()?)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && entry.path().join("job.json").is_file() {
            jobs.push(read::<Job>(&entry.path().join("job.json"))?);
        }
    }
    jobs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_cannot_become_options_or_shell_syntax() {
        for s in ["", "--project=other", "a;rm", "$(cmd)", "a\ncommand"] {
            assert!(token(s).is_err());
        }
        assert!(token("projects/my-project/global/networks/default").is_ok());
    }
    #[test]
    fn request_ids_are_nonzero_v4_uuids() {
        let a = uuid().unwrap();
        let b = uuid().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert!("89ab".contains(&a[19..20]));
    }
}
