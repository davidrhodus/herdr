//! Per-target, client-local SSH enrollment. Only a public key crosses SSH.
use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
    Engine as _,
};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

fn captured(mut command: Command) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = super::bounded_discovery_output(
        command.spawn()?,
        Duration::from_secs(30),
        Arc::new(AtomicBool::new(false)),
        256 * 1024,
    )?;
    if output.stdout.len() > 256 * 1024 || output.stderr.len() > 256 * 1024 {
        return Err(io::Error::other(
            "SSH key setup produced too much diagnostic output",
        ));
    }
    Ok(output)
}

fn key_directory(root: &Path, target: &str) -> PathBuf {
    root.join("client/ssh-keys")
        .join(format!("{:x}", Sha256::digest(target.as_bytes())))
}

fn ensure_directory(path: &Path) -> io::Result<()> {
    match crate::platform::create_remote_private_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    crate::platform::validate_private_key_path(path, true)
}

struct GenerationDirectory(PathBuf);
impl Drop for GenerationDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn generate_key(directory: &Path, private: &Path) -> io::Result<()> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let temporary = directory.join(format!(
        ".generate-{}",
        STANDARD_NO_PAD.encode(random).replace('/', "_")
    ));
    crate::platform::create_remote_private_dir(&temporary)?;
    let temporary = GenerationDirectory(temporary);
    let staged = temporary.0.join("id_ed25519");
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "herdr-managed", "-f"])
        .arg(&staged)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(io::Error::other(
            "ssh-keygen failed; existing keys were not changed",
        ));
    }
    crate::platform::validate_private_key_path(&staged, false)?;
    // Publish only a complete, readable key. A canceled generation can be retried.
    public_key(&staged)?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&staged)?
        .sync_all()?;
    fs::rename(&staged, private)?;
    crate::platform::sync_parent_directory(directory)?;
    Ok(())
}

fn public_key(private: &Path) -> io::Result<(String, String)> {
    let mut command = Command::new("ssh-keygen");
    command.args(["-y", "-P", "", "-f"]).arg(private);
    let output = captured(command)?;
    if !output.status.success() {
        return Err(io::Error::other(
            "Could not read the dedicated SSH key; existing keys were not changed",
        ));
    }
    parse_public_key(&output.stdout)
}

fn parse_public_key(bytes: &[u8]) -> io::Result<(String, String)> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let fields: Vec<_> = text.split_whitespace().collect();
    let invalid = || io::Error::other("Expected an OpenSSH Ed25519 public key");
    // OpenSSH versions differ on whether -y retains the private key's comment.
    // Accept one public-key line and discard the comment before shell quoting.
    if fields.len() < 2 || fields[0] != "ssh-ed25519" || text.trim().lines().count() != 1 {
        return Err(invalid());
    }
    let blob = STANDARD.decode(fields[1]).map_err(|_| invalid())?;
    if blob.len() != 51 || &blob[..15] != b"\0\0\0\x0bssh-ed25519" || &blob[15..19] != b"\0\0\0\x20"
    {
        return Err(invalid());
    }
    let fingerprint = format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&blob)));
    Ok((
        format!("ssh-ed25519 {} herdr-managed", STANDARD.encode(blob)),
        fingerprint,
    ))
}

fn install_script(public: &str) -> String {
    format!(
        r#"set -eu
umask 077
case "$(uname -s)" in Linux|Darwin|FreeBSD|OpenBSD|NetBSD) ;; *) echo 'SSH key setup requires a Unix SSH server' >&2; exit 1 ;; esac
[ -n "${{HOME:-}}" ] && [ "$HOME" != / ] || exit 1
dir="$HOME/.ssh"
file="$dir/authorized_keys"
[ ! -L "$dir" ] && [ ! -L "$file" ] || {{ echo 'Refusing symlinked SSH key storage' >&2; exit 1; }}
mkdir -p "$dir"
chmod 700 "$dir"
if [ ! -e "$file" ]; then (set -C; : > "$file"); fi
[ -f "$file" ] && [ -w "$file" ] || {{ echo 'Cannot update authorized_keys for this account' >&2; exit 1; }}
chmod 600 "$file"
key={key}
blob={blob}
if ! awk -v blob="$blob" '$1 == "ssh-ed25519" && $2 == blob {{ found=1 }} END {{ exit !found }}' "$file"; then
    printf '\n%s\n' "$key" >> "$file"
fi
printf 'Herdr public key installed.\n'
"#,
        key = super::shell_quote(public),
        blob = super::shell_quote(public.split_whitespace().nth(1).unwrap_or_default())
    )
}

fn verification_command(target: &str, private: &Path) -> Command {
    let mut command = Command::new("ssh");
    // These options precede the user's config (OpenSSH uses the first value).
    // Never let a cached password-authenticated master make this test pass.
    command
        .args([
            "-v",
            "-o",
            "BatchMode=yes",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPath=none",
            "-o",
            "ControlPersist=no",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "IdentityAgent=none",
            "-o",
            "PreferredAuthentications=publickey",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ConnectionAttempts=1",
            "-o",
            "NumberOfPasswordPrompts=0",
            "-i",
        ])
        .arg(private)
        .args(["-T", target, "exit"])
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env_remove("SSH_ASKPASS");
    command
}

fn authenticated_with(diagnostics: &str, fingerprint: &str) -> bool {
    let mut accepted = false;
    let mut authenticated = false;
    for line in diagnostics.lines() {
        if line.contains("Server accepts key:") {
            accepted = line.split_whitespace().any(|part| part == fingerprint);
        }
        if line.contains("Authenticated to ") && line.contains("using \"publickey\"")
            || line.contains("Authentication succeeded (publickey)")
        {
            authenticated = accepted;
        }
    }
    authenticated
}

fn verify(target: &str, private: &Path, fingerprint: &str) -> io::Result<bool> {
    let output = captured(verification_command(target, private))?;
    Ok(output.status.success()
        && authenticated_with(&String::from_utf8_lossy(&output.stderr), fingerprint))
}

pub(crate) fn run(target: &str) -> io::Result<PathBuf> {
    super::validate_remote_target(target)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let root = crate::config::state_dir();
    fs::create_dir_all(root.join("client"))?;
    ensure_directory(&root.join("client/ssh-keys"))?;
    let directory = key_directory(&root, target);
    ensure_directory(&directory)?;
    let lock_path = directory.join("setup.lock");
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
    lock.try_lock()
        .map_err(|_| io::Error::other("SSH key setup is already running for this server"))?;
    let private = directory.join("id_ed25519");
    match fs::symlink_metadata(&private) {
        Ok(_) => crate::platform::validate_private_key_path(&private, false)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            println!("Generating a dedicated Ed25519 key on this computer…");
            generate_key(&directory, &private)?;
        }
        Err(error) => return Err(error),
    }
    let (public, fingerprint) = public_key(&private)?;
    println!("Checking password-free SSH access…");
    if !verify(target, &private, &fingerprint)? {
        println!("Installing the public key. Enter the existing SSH password if requested.");
        let script = install_script(&public);
        let remote = format!("/bin/sh -c {}", super::shell_quote(&script));
        let mut install = super::attach::ssh_interactive_command(target, &remote)?;
        if !install.command.status()?.success() {
            return Err(io::Error::other("Public key installation failed; the saved SSH identity was not changed. Retry to reuse this key."));
        }
        println!("Verifying a fresh connection with the new key…");
        if !verify(target, &private, &fingerprint)? {
            return Err(io::Error::other("The public key was installed, but password-free login could not be verified. The saved SSH identity was not changed; retry after checking SSH public-key access on the server."));
        }
    }
    super::device_identity::set(target, &private.to_string_lossy()).map_err(io::Error::other)?;
    println!("SSH key ready ({fingerprint}). Herdr will use it automatically on this computer.");
    println!("Open clients retry within 30 seconds. Private keys stay local and are never synced.");
    Ok(private)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_requires_the_new_key_and_a_fresh_public_key_login() {
        let ours = "SHA256:ours";
        assert!(!authenticated_with(
            "Authenticated to host using \"password\".",
            ours
        ));
        assert!(!authenticated_with(
            "Authenticated to host using \"publickey\".",
            ours
        ));
        let accepted = "debug1: Server accepts key: /private/key ED25519 SHA256:ours explicit\n";
        assert!(!authenticated_with(accepted, ours));
        assert!(authenticated_with(
            &format!("{accepted}Authenticated to host ([ip]:22) using \"publickey\"."),
            ours
        ));
        assert!(authenticated_with(
            &format!("{accepted}debug1: Authentication succeeded (publickey)."),
            ours
        ));
        assert!(!authenticated_with(&format!("{accepted}debug1: Server accepts key: /other ED25519 SHA256:other explicit\nAuthenticated to host using \"publickey\"."),ours));
        let command = verification_command("david@redie", Path::new("/private/key"));
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for option in [
            "ControlPath=none",
            "ControlMaster=no",
            "BatchMode=yes",
            "IdentityAgent=none",
            "IdentitiesOnly=yes",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "PreferredAuthentications=publickey",
            "StrictHostKeyChecking=yes",
        ] {
            assert!(args.iter().any(|arg| arg == option), "{option}");
        }
    }

    #[test]
    fn malformed_public_keys_cannot_reach_the_remote_script() {
        for value in [
            b"ssh-rsa AAAA".as_slice(),
            b"ssh-ed25519 ';touch /tmp/unsafe",
            b"ssh-ed25519 AAAA",
            b"\xff",
        ] {
            assert!(parse_public_key(value).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn generated_key_and_remote_install_are_private_idempotent_and_preserve_existing_keys() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory = crate::platform::create_remote_ssh_config_dir("key-test").unwrap();
        let cleanup = GenerationDirectory(directory.clone());
        let private = directory.join("identity");
        generate_key(&directory, &private).unwrap();
        let (public, fingerprint) = public_key(&private).unwrap();
        assert!(fingerprint.starts_with("SHA256:"));
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(fs::read_dir(&directory).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".generate-")));
        let remote = directory.join("remote .ssh");
        fs::create_dir(&remote).unwrap();
        let authorized = remote.join("authorized_keys");
        let existing = b"existing-key-without-newline";
        fs::write(&authorized, existing).unwrap();
        let script = install_script(&public).replace(
            "dir=\"$HOME/.ssh\"",
            &format!(
                "dir={}",
                super::super::shell_quote(&remote.to_string_lossy())
            ),
        );
        for _ in 0..2 {
            assert!(Command::new("/bin/sh")
                .args(["-c", &script])
                .stdout(Stdio::null())
                .status()
                .unwrap()
                .success());
        }
        let saved = fs::read_to_string(&authorized).unwrap();
        assert!(saved.starts_with("existing-key-without-newline\n"));
        assert_eq!(saved.matches(&public).count(), 1);
        assert_eq!(
            fs::metadata(&authorized).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&remote).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_file(&authorized).unwrap();
        let outside = directory.join("outside");
        fs::write(&outside, b"must remain").unwrap();
        symlink(&outside, &authorized).unwrap();
        assert!(!Command::new("/bin/sh")
            .args(["-c", &script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());
        assert_eq!(fs::read(outside).unwrap(), b"must remain");
        drop(cleanup);
    }
}
