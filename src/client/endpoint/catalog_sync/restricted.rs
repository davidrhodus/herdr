//! Forced-command dispatcher for keys that may only exchange this registry.
use super::*;
pub(crate) const STATUS: &str = "herdr-sync-status-v1";
pub(crate) const EXCHANGE: &str = "herdr-sync-exchange-v1";
pub(crate) const MARKER: &str = "herdr-remote-output-ready:1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enrollment {
    public_key: String,
}

pub(crate) fn authorize_stdio() -> io::Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin().lock().take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(io::Error::other(
            "Public key enrollment request is too large",
        ));
    }
    let request: Enrollment = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let (public, fingerprint) =
        crate::remote::key_setup::parse_public_key(request.public_key.as_bytes())?;
    let mut parts = public.split_whitespace();
    let public = format!(
        "{} {} herdr-sync-only",
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default()
    );
    let root = directory();
    let _guard = lock(&root).map_err(io::Error::other)?;
    let state = load(&root)
        .map_err(io::Error::other)?
        .filter(|state| state.source == Source::Local)
        .ok_or_else(|| io::Error::other("This account is not hosting an opted-in shared list"))?;
    let exe = std::fs::canonicalize(std::env::current_exe()?)?;
    let root = std::fs::canonicalize(root)?;
    crate::platform::sync_keys::install(&exe, &root, &state.device, &public, &fingerprint)?;
    println!("\n{MARKER}");
    println!(
        "{}",
        serde_json::json!({"registry_id": state.device, "key_fingerprint": fingerprint})
    );
    Ok(())
}

pub(crate) fn serve(root: &Path, registry: &str, fingerprint: &str) -> io::Result<()> {
    let command = std::env::var("SSH_ORIGINAL_COMMAND").unwrap_or_default();
    dispatch(
        root,
        registry,
        fingerprint,
        &command,
        std::env::var("SSH_CONNECTION").ok().as_deref(),
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
    )
    .map_err(io::Error::other)
}

fn dispatch(
    root: &Path,
    registry: &str,
    fingerprint: &str,
    command: &str,
    connection: Option<&str>,
    input: &mut impl io::Read,
    output: &mut impl io::Write,
) -> Result<(), String> {
    if !root.is_absolute() || !herdr_sync::valid_id(registry) {
        return Err("Invalid restricted registry binding".into());
    }
    // Exact request names only. Never evaluate or forward SSH_ORIGINAL_COMMAND.
    match command {
        STATUS => {
            let _guard = lock(root)?;
            let state = load(root)?
                .filter(|state| state.source == Source::Local && state.device == registry)
                .ok_or("Shared registry changed or is disabled")?;
            writeln!(output, "\n{MARKER}").map_err(|e| e.to_string())?;
            serde_json::to_writer(&mut *output, &serde_json::json!({"enabled":true,"source":{"kind":"local"},"registry_id":state.device,"key_fingerprint":fingerprint,"restricted_sync_v1":true})).map_err(|e| e.to_string())?;
            output.flush().map_err(|e| e.to_string())
        }
        EXCHANGE => {
            let mut bytes = Vec::new();
            input
                .take(MAX_BODY as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > MAX_BODY {
                return Err("Catalog sync input exceeds size limit".into());
            }
            let incoming: Document =
                serde_json::from_slice(&bytes).map_err(|_| "Invalid catalog sync request")?;
            writeln!(output, "\n{MARKER}").map_err(|e| e.to_string())?;
            reply_exchange_bound(
                root,
                &incoming,
                connection,
                output,
                Some((registry, fingerprint)),
            )
        }
        _ => Err("HERDR_SYNC_ONLY_DENIED: this SSH key only permits server-list sync".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Temp;
    use super::*;
    #[test]
    fn fixed_requests_work_and_shell_file_transfer_and_injected_args_are_denied() {
        let temp = Temp::new();
        let registry = "a".repeat(32);
        save(
            &temp.0,
            &SyncState {
                version: 1,
                source: Source::Local,
                device: registry.clone(),
                document: Document::default(),
                baseline: vec![],
                pending_projection: None,
                last_success: None,
                last_error: None,
            },
        )
        .unwrap();
        let mut output = Vec::new();
        dispatch(
            &temp.0,
            &registry,
            "fingerprint",
            STATUS,
            None,
            &mut &b""[..],
            &mut output,
        )
        .unwrap();
        assert!(String::from_utf8(output).unwrap().contains(&registry));
        let document = serde_json::to_vec(&Document::default()).unwrap();
        dispatch(
            &temp.0,
            &registry,
            "fingerprint",
            EXCHANGE,
            None,
            &mut document.as_slice(),
            &mut Vec::new(),
        )
        .unwrap();
        for command in [
            "",
            "id",
            "sh",
            "scp -t /tmp/file",
            "internal-sftp",
            "herdr-sync-status-v1 --extra",
            "herdr-sync-status-v1; id",
            "herdr-sync-exchange-v1\nid",
            "herdr-sync-status-v1 ",
        ] {
            assert!(dispatch(
                &temp.0,
                &registry,
                "fingerprint",
                command,
                None,
                &mut &b""[..],
                &mut Vec::new()
            )
            .is_err());
        }
    }
    #[test]
    fn restricted_key_cannot_follow_a_rehosted_registry_or_oversized_input() {
        let temp = Temp::new();
        save(
            &temp.0,
            &SyncState {
                version: 1,
                source: Source::Local,
                device: "a".repeat(32),
                document: Document::default(),
                baseline: vec![],
                pending_projection: None,
                last_success: None,
                last_error: None,
            },
        )
        .unwrap();
        let bytes = serde_json::to_vec(&Document::default()).unwrap();
        assert!(dispatch(
            &temp.0,
            &"b".repeat(32),
            "fp",
            EXCHANGE,
            None,
            &mut bytes.as_slice(),
            &mut Vec::new()
        )
        .is_err());
        assert!(dispatch(
            &temp.0,
            &"b".repeat(32),
            "fp",
            STATUS,
            None,
            &mut &b""[..],
            &mut Vec::new()
        )
        .is_err());
        assert!(dispatch(
            &temp.0,
            &"a".repeat(32),
            "fp",
            EXCHANGE,
            None,
            &mut vec![b' '; MAX_BODY + 1].as_slice(),
            &mut Vec::new()
        )
        .is_err());
        assert!(load(&temp.0).unwrap().unwrap().document.records.is_empty());
    }
}
