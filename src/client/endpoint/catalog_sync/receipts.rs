//! Host-side diagnostics for existing v1 clients. No wire or journal changes.
use super::*;
use std::net::IpAddr;

const MAX_RECEIPTS: usize = 64;
const MAX_BYTES: u64 = 32 * 1024;
const RECENT_SECONDS: u64 = 60;
const FILE: &str = "catalog-manager-receipts.json";

#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    registry: String,
    address: IpAddr,
    last_exchange: u64,
    request_digest: String,
    response_digest: String,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn digest(document: &Document) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    // Record order is not significant in v1 documents.
    let canonical = document.merge(&Document::default())?;
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical).map_err(|e| e.to_string())?)
    ))
}

fn address(connection: &str) -> Option<IpAddr> {
    let fields: Vec<_> = connection.split_whitespace().collect();
    if fields.len() != 4
        || fields[1].parse::<u16>().ok()? == 0
        || fields[3].parse::<u16>().ok()? == 0
    {
        return None;
    }
    fields[2].parse::<IpAddr>().ok()?;
    fields[0].parse().ok()
}

fn load(root: &Path) -> Result<Vec<Receipt>, String> {
    let file = match File::open(root.join(FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("manager receipts exceed size limit".into());
    }
    let receipts: Vec<Receipt> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if receipts.len() > MAX_RECEIPTS {
        return Err("too many manager receipts".into());
    }
    Ok(receipts)
}

pub(super) fn record(
    root: &Path,
    registry: &str,
    connection: &str,
    incoming: &Document,
    response: &Document,
) -> Result<(), String> {
    let Some(address) = address(connection) else {
        return Ok(());
    };
    let receipt = Receipt {
        registry: registry.into(),
        address,
        last_exchange: now(),
        request_digest: digest(incoming)?,
        response_digest: digest(response)?,
    };
    let _guard = lock(root)?;
    let Some(state) = super::load(root)? else {
        return Ok(());
    };
    if state.source != Source::Local || state.device != registry {
        return Ok(());
    }
    // A damaged diagnostics file must not prevent future receipts or sync.
    let mut receipts = load(root).unwrap_or_default();
    receipts.retain(|old| old.registry == registry && old.address != address);
    receipts.insert(0, receipt);
    receipts.sort_by_key(|r| std::cmp::Reverse(r.last_exchange));
    receipts.truncate(MAX_RECEIPTS);
    super::super::store_private_json(
        &root.join(FILE),
        &serde_json::to_vec(&receipts).map_err(|e| e.to_string())?,
        "manager exchange receipts",
    )
}

// Caller already holds the catalog lock. Stale or divergent receipts never
// claim convergence, even when an earlier response was delivered successfully.
pub(super) fn status(
    root: &Path,
    registry: &str,
    current: &Document,
    catalog_matches: bool,
) -> serde_json::Value {
    let current = match digest(current) {
        Ok(digest) => digest,
        Err(_) => return serde_json::json!([]),
    };
    let now = now();
    serde_json::Value::Array(load(root).unwrap_or_default().into_iter().filter(|receipt| receipt.registry == registry).map(|receipt| {
        let recent = receipt.last_exchange <= now && now - receipt.last_exchange <= RECENT_SECONDS;
        serde_json::json!({"address": receipt.address, "last_exchange": receipt.last_exchange, "recent": recent,
            "in_sync": catalog_matches && recent && receipt.request_digest == current && receipt.response_digest == current})
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("herdr-sync-receipts-{}", ProfileId::generate()));
            std::fs::create_dir_all(&path).unwrap();
            save(
                &path,
                &SyncState {
                    version: 1,
                    source: Source::Local,
                    device: "d".repeat(32),
                    document: Document::default(),
                    baseline: vec![],
                    pending_projection: None,
                    last_success: None,
                    last_error: None,
                },
            )
            .unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn doc() -> Document {
        Document {
            version: 1,
            records: vec![Record {
                id: "a".repeat(32),
                stamp: Stamp {
                    counter: 1,
                    device: "b".repeat(32),
                },
                machine: None,
            }],
        }
    }
    #[test]
    fn readback_is_required_before_reporting_convergence() {
        let temp = Temp::new();
        let document = doc();
        let connection = "100.79.132.38 12345 100.109.85.54 22";
        record(
            &temp.0,
            &"d".repeat(32),
            connection,
            &Document::default(),
            &document,
        )
        .unwrap();
        assert_eq!(
            status(&temp.0, &"d".repeat(32), &document, true)[0]["in_sync"],
            false
        );
        record(&temp.0, &"d".repeat(32), connection, &document, &document).unwrap();
        let status = status(&temp.0, &"d".repeat(32), &document, true);
        assert_eq!(status.as_array().unwrap().len(), 1);
        assert_eq!(status[0]["in_sync"], true);
        assert_eq!(status[0]["address"], "100.79.132.38");
        assert_eq!(
            super::status(&temp.0, &"d".repeat(32), &Document::default(), true)[0]["in_sync"],
            false
        );
    }
    #[test]
    fn stale_and_future_receipts_do_not_claim_live_sync() {
        let temp = Temp::new();
        let document = doc();
        record(
            &temp.0,
            &"d".repeat(32),
            "::1 12345 ::1 22",
            &document,
            &document,
        )
        .unwrap();
        let mut receipts = load(&temp.0).unwrap();
        for at in [now() - RECENT_SECONDS - 1, now() + 3600] {
            receipts[0].last_exchange = at;
            std::fs::write(temp.0.join(FILE), serde_json::to_vec(&receipts).unwrap()).unwrap();
            assert_eq!(
                status(&temp.0, &"d".repeat(32), &document, true)[0]["recent"],
                false
            );
            assert_eq!(
                status(&temp.0, &"d".repeat(32), &document, true)[0]["in_sync"],
                false
            );
        }
    }
    #[test]
    fn invalid_connections_are_ignored_and_receipts_remain_bounded() {
        let temp = Temp::new();
        let document = doc();
        for connection in [
            "",
            "hostname 1 ::1 22",
            "::1 0 ::1 22",
            "::1 1 ::1 65536",
            "::1 1 ::1 22 extra",
        ] {
            record(&temp.0, &"d".repeat(32), connection, &document, &document).unwrap();
        }
        assert!(load(&temp.0).unwrap().is_empty());
        for n in 1..=70 {
            record(
                &temp.0,
                &"d".repeat(32),
                &format!("100.64.0.{n} 12345 100.109.85.54 22"),
                &document,
                &document,
            )
            .unwrap();
        }
        assert_eq!(load(&temp.0).unwrap().len(), MAX_RECEIPTS);
    }
    #[test]
    fn damaged_or_oversized_diagnostics_do_not_block_recovery() {
        let temp = Temp::new();
        let document = doc();
        for bytes in [b"broken".to_vec(), vec![b' '; MAX_BYTES as usize + 1]] {
            std::fs::write(temp.0.join(FILE), bytes).unwrap();
            assert_eq!(
                status(&temp.0, &"d".repeat(32), &document, true),
                serde_json::json!([])
            );
            record(
                &temp.0,
                &"d".repeat(32),
                "100.79.132.38 12345 100.109.85.54 22",
                &document,
                &document,
            )
            .unwrap();
            assert_eq!(
                status(&temp.0, &"d".repeat(32), &document, true)[0]["in_sync"],
                true
            );
        }
    }
    #[test]
    fn digest_ignores_record_order() {
        let mut a = doc();
        let mut record = a.records[0].clone();
        record.id = "c".repeat(32);
        a.records.push(record);
        let mut b = a.clone();
        b.records.reverse();
        assert_eq!(digest(&a).unwrap(), digest(&b).unwrap());
    }

    #[test]
    fn rehosting_and_unpublished_local_edits_invalidate_receipts() {
        let temp = Temp::new();
        let document = doc();
        let registry = "d".repeat(32);
        let connection = "100.79.132.38 12345 100.109.85.54 22";
        record(&temp.0, &registry, connection, &document, &document).unwrap();
        assert_eq!(
            status(&temp.0, &registry, &document, false)[0]["in_sync"],
            false
        );
        let mut state = super::super::load(&temp.0).unwrap().unwrap();
        state.device = "e".repeat(32);
        save(&temp.0, &state).unwrap();
        record(&temp.0, &registry, connection, &document, &document).unwrap();
        assert_eq!(
            status(&temp.0, &state.device, &document, true),
            serde_json::json!([])
        );
        assert_eq!(load(&temp.0).unwrap().len(), 1);
    }

    #[test]
    fn failed_response_or_flush_never_records_success() {
        struct FailingOutput(bool);
        impl std::io::Write for FailingOutput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.0 {
                    Err(io::Error::other("broken output"))
                } else {
                    Ok(bytes.len())
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("failed flush"))
            }
        }
        let temp = Temp::new();
        for fail_write in [false, true] {
            assert!(super::super::reply_exchange(
                &temp.0,
                &Document::default(),
                Some("100.79.132.38 12345 100.109.85.54 22"),
                &mut FailingOutput(fail_write)
            )
            .is_err());
            assert!(load(&temp.0).unwrap().is_empty());
        }
        super::super::reply_exchange(&temp.0, &Document::default(), None, &mut Vec::new()).unwrap();
        assert!(load(&temp.0).unwrap().is_empty());
    }
}
