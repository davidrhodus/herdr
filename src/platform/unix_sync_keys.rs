//! Per-key OpenSSH authorization. Existing login keys are never edited.
use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
};
pub(crate) const SUPPORTED: bool = true;
const MAX_AUTHORIZED_BYTES: u64 = 2 * 1024 * 1024;

fn entry(
    exe: &Path,
    root: &Path,
    registry: &str,
    public: &str,
    fingerprint: &str,
) -> io::Result<String> {
    let exe = exe
        .to_str()
        .ok_or_else(|| io::Error::other("SSH executable path must be UTF-8"))?;
    let root = root
        .to_str()
        .ok_or_else(|| io::Error::other("SSH registry path must be UTF-8"))?;
    if !Path::new(exe).is_absolute()
        || !Path::new(root).is_absolute()
        || [exe, root, registry, public, fingerprint]
            .iter()
            .any(|s| s.chars().any(char::is_control))
    {
        return Err(io::Error::other("Invalid restricted SSH authorization"));
    }
    let command = [exe, "sync", "restricted-v1", root, registry, fingerprint]
        .map(crate::remote::shell_quote)
        .join(" ");
    // OpenSSH opt_dequote unescapes only \". Other backslashes must remain
    // literal, especially the POSIX '\'' sequence used for apostrophes.
    let command = command.replace('"', "\\\"");
    let line = format!("restrict,command=\"{command}\" {public}");
    if line.len() > 8 * 1024 {
        return Err(io::Error::other("Restricted SSH authorization is too long"));
    }
    Ok(line)
}

pub(crate) fn install(
    exe: &Path,
    root: &Path,
    registry: &str,
    public: &str,
    fingerprint: &str,
) -> io::Result<()> {
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is unavailable"))?;
    let directory = std::path::PathBuf::from(home).join(".ssh");
    match super::create_remote_private_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    super::validate_private_key_path(&directory, true)?;
    append(
        &directory.join("authorized_keys"),
        &entry(exe, root, registry, public, fingerprint)?,
        public
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| io::Error::other("Missing public key"))?,
    )
}

fn append(path: &Path, line: &str, blob: &str) -> io::Result<()> {
    match super::create_private_state_file(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    super::validate_private_key_path(path, false)?;
    // Lock the opened file and append one bounded line. O_APPEND preserves
    // unrelated concurrent appends (including existing SSH key-setup tools).
    let mut file = fs::OpenOptions::new().read(true).append(true).open(path)?;
    file.lock()?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_AUTHORIZED_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_AUTHORIZED_BYTES {
        return Err(io::Error::other("authorized_keys exceeds size limit"));
    }
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let matches: Vec<_> = text
        .lines()
        .filter(|existing| existing.split_whitespace().any(|token| token == blob))
        .collect();
    if !matches.is_empty() {
        if matches.len() == 1 && matches[0] == line {
            return Ok(());
        }
        return Err(io::Error::other("This sync public key already has a different authorization; refusing duplicate or unrestricted access"));
    }
    let append = format!("\n{line}\n");
    if bytes.len() as u64 + append.len() as u64 > MAX_AUTHORIZED_BYTES {
        return Err(io::Error::other("authorized_keys exceeds size limit"));
    }
    file.write_all(append.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forced_command_quotes_shell_and_authorized_key_layers() {
        let line = entry(
            Path::new("/home/a $x/quote'\"/herdr"),
            Path::new("/private/state $x'\""),
            &"a".repeat(32),
            "ssh-ed25519 AAAA herdr-sync-only",
            "SHA256:fp",
        )
        .unwrap();
        assert!(line.starts_with("restrict,command=\""));
        assert!(line.contains("sync restricted-v1"));
        assert!(line.contains("\\\""));
        assert!(!line.contains('\n'));
        assert!(entry(
            Path::new("/bad\npath"),
            Path::new("/root"),
            "id",
            "key",
            "fp"
        )
        .is_err());
    }
    #[test]
    fn append_is_idempotent_preserves_other_keys_and_refuses_duplicate_privileges() {
        let dir =
            std::env::temp_dir().join(format!("herdr-restricted-auth-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("authorized_keys");
        let line = "restrict,command=\"literal\" ssh-ed25519 restricted-blob herdr-sync-only";
        append(&path, line, "restricted-blob").unwrap();
        append(&path, line, "restricted-blob").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().matches(line).count(), 1);
        fs::write(&path, "ssh-ed25519 other-blob existing-login\nssh-ed25519 restricted-blob existing-full-access\n").unwrap();
        assert!(append(&path, line, "restricted-blob").is_err());
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("existing-login"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn authorized_keys_dequoting_preserves_literal_shell_arguments() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "herdr-force-command-{}",
            crate::client::endpoint::ProfileId::generate()
        ));
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("program $literal 'quote\"\\backslash");
        fs::write(&exe, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        let root = dir.join("state $literal 'quote\"\\backslash");
        let registry = "a".repeat(32);
        let line = entry(
            &exe,
            &root,
            &registry,
            "ssh-ed25519 AAAA herdr-sync-only",
            "SHA256:fp",
        )
        .unwrap();
        // Match OpenSSH misc.c opt_dequote: only backslash-double-quote is
        // decoded; a backslash before any other character remains literal.
        let mut chars = line
            .strip_prefix("restrict,command=\"")
            .unwrap()
            .chars()
            .peekable();
        let mut command = String::new();
        while let Some(c) = chars.next() {
            if c == '"' {
                break;
            }
            if c == '\\' && chars.peek() == Some(&'"') {
                command.push(chars.next().unwrap());
            } else {
                command.push(c);
            }
        }
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &command])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec![
                "sync",
                "restricted-v1",
                root.to_str().unwrap(),
                &registry,
                "SHA256:fp"
            ]
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
