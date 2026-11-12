//! Bounded host observations, independent of rendering and terminal codecs.
use super::*;
use std::net::IpAddr;
const FILE: &str = "catalog-host-status.json";
const LIMIT: u64 = 96 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manager {
    pub address: IpAddr,
    pub last_exchange: u64,
    pub in_sync: bool,
    pub authentication: String,
    pub key_fingerprint: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Report {
    pub checked_at: u64,
    pub catalog_digest: String,
    pub managers: Vec<Manager>,
}
impl Report {
    fn validate(&self) -> Result<(), String> {
        if self.checked_at == 0
            || self.catalog_digest.len() != 64
            || !self.catalog_digest.bytes().all(|b| b.is_ascii_hexdigit())
            || self.managers.len() > 64
        {
            return Err("Invalid manager status report".into());
        }
        for row in &self.managers {
            if row.authentication.len() > 32
                || row.authentication.chars().any(char::is_control)
                || (row.authentication == "restricted_key" && row.key_fingerprint.is_none())
                || row.key_fingerprint.as_ref().is_some_and(|fp| {
                    fp.len() != 50 || !fp.starts_with("SHA256:") || fp.chars().any(char::is_control)
                })
            {
                return Err("Invalid manager credential report".into());
            }
        }
        Ok(())
    }
    fn from_value(value: serde_json::Value) -> Result<Self, String> {
        if value["manager_report_v1"] != true {
            return Err("Update the shared-list host to show manager sync status".into());
        }
        let report: Self =
            serde_json::from_value(value).map_err(|_| "Invalid manager status report")?;
        report.validate()?;
        Ok(report)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    version: u32,
    source: Source,
    device: String,
    fetched_at: u64,
    report: Option<Report>,
    error: Option<String>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn read(root: &Path) -> Result<Option<Cache>, String> {
    let file = match File::open(root.join(FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("Manager status cache exceeds size limit".into());
    }
    let cache: Cache =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid manager status cache")?;
    if cache.version != 1 || !herdr_sync::valid_id(&cache.device) {
        return Err("Invalid manager status cache version".into());
    }
    cache.source.validate()?;
    if let Some(report) = &cache.report {
        report.validate()?;
    }
    Ok(Some(cache))
}
pub(crate) fn refresh() -> Result<(), String> {
    refresh_at(&directory())
}
pub(super) fn refresh_at(root: &Path) -> Result<(), String> {
    let lease = crate::client::direct_sync::worker_lease(root, "catalog-status-refresh.lock")
        .map_err(|e| e.to_string())?;
    if lease.try_lock().is_err() {
        return Ok(());
    }
    fetch_at(root)
}
pub(super) fn schedule_at(root: &Path) {
    let Ok(lease) = crate::client::direct_sync::worker_lease(root, "catalog-status-refresh.lock")
    else {
        return;
    };
    if lease.try_lock().is_err() {
        return;
    }
    let root = root.to_owned();
    std::thread::spawn(move || {
        let _lease = lease;
        let _ = fetch_at(&root);
    });
}
fn fetch_at(root: &Path) -> Result<(), String> {
    refresh_with(root, |target| {
        crate::remote::catalog_host_report(target)
            .map_err(|e| e.to_string())
            .and_then(Report::from_value)
    })
}
fn refresh_with(
    root: &Path,
    fetch: impl FnOnce(&str) -> Result<Report, String>,
) -> Result<(), String> {
    let (device, source) = {
        let _guard = lock(root)?;
        let Some(state) = load(root)? else {
            return Ok(());
        };
        if !matches!(state.source, Source::Ssh { .. }) {
            return Ok(());
        }
        (state.device, state.source)
    };
    let Source::Ssh { target } = &source else {
        return Ok(());
    };
    let result = fetch(target).and_then(|report| {
        report.validate()?;
        Ok(report)
    });
    let _guard = lock(root)?;
    if !load(root)?.is_some_and(|state| state.device == device && state.source == source) {
        return Ok(());
    }
    let mut cache = read(root)
        .ok()
        .flatten()
        .filter(|cache| cache.device == device && cache.source == source)
        .unwrap_or(Cache {
            version: 1,
            source,
            device,
            fetched_at: 0,
            report: None,
            error: None,
        });
    match result {
        Ok(report) => {
            cache.report = Some(report);
            cache.fetched_at = now();
            cache.error = None;
        }
        Err(error) => {
            cache.error = Some(
                error
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(2048)
                    .collect(),
            );
        }
    }
    let bytes = serde_json::to_vec(&cache).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("Manager status cache exceeds size limit".into());
    }
    super::super::store_private_json(&root.join(FILE), &bytes, "manager sync status")
}

pub(crate) struct Snapshot {
    pub source: Option<Source>,
    pub automatic: bool,
    pub own_fingerprint: Option<String>,
    pub local_digest: Option<String>,
    pub pending: bool,
    pub error: Option<String>,
    pub report: Option<Report>,
    pub fetched_at: u64,
}
pub(crate) fn snapshot() -> Result<Snapshot, String> {
    snapshot_at(&directory())
}
fn snapshot_at(root: &Path) -> Result<Snapshot, String> {
    let _guard = lock(root)?;
    let mut snapshot = Snapshot {
        source: None,
        automatic: !automatic::disabled(root),
        own_fingerprint: None,
        local_digest: None,
        pending: false,
        error: None,
        report: None,
        fetched_at: 0,
    };
    let Some(state) = load(root)? else {
        return Ok(snapshot);
    };
    snapshot.local_digest = Some(receipts::digest(&state.document)?);
    let current = EndpointCatalog::load_from_path(&root.join("endpoints.json"))?;
    snapshot.pending = state.pending_projection.is_some() || current.ssh != state.baseline;
    snapshot.error = state.last_error;
    match &state.source {
        Source::Ssh { target } => {
            snapshot.own_fingerprint = crate::remote::sync_key_status(target)["fingerprint"]
                .as_str()
                .map(str::to_owned);
            match read(root) {
                Ok(Some(cache)) if cache.device == state.device && cache.source == state.source => {
                    snapshot.report = cache.report;
                    snapshot.fetched_at = cache.fetched_at;
                    if snapshot.error.is_none() {
                        snapshot.error = cache.error;
                    }
                }
                Err(error) => snapshot.error = Some(error),
                _ => {}
            }
        }
        Source::Local => {
            let managers =
                receipts::status(root, &state.device, &state.document, !snapshot.pending);
            let report = Report {
                checked_at: now(),
                catalog_digest: receipts::digest(&state.document)?,
                managers: serde_json::from_value(managers).map_err(|e| e.to_string())?,
            };
            report.validate()?;
            snapshot.report = Some(report);
            snapshot.fetched_at = now();
        }
        Source::Direct => {}
    }
    snapshot.source = Some(state.source);
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::super::tests::Temp;
    use super::*;
    fn setup(root: &Path) {
        save(
            root,
            &SyncState {
                version: 1,
                source: Source::Ssh {
                    target: "redie".into(),
                },
                device: "a".repeat(32),
                document: Document::default(),
                baseline: vec![],
                pending_projection: None,
                last_success: None,
                last_error: None,
            },
        )
        .unwrap();
    }
    fn report() -> Report {
        Report {
            checked_at: 1000,
            catalog_digest: "a".repeat(64),
            managers: vec![],
        }
    }
    #[test]
    fn failed_refresh_keeps_old_timestamp_and_observation() {
        let t = Temp::new();
        setup(&t.0);
        refresh_with(&t.0, |_| Ok(report())).unwrap();
        let old = read(&t.0).unwrap().unwrap();
        refresh_with(&t.0, |_| Err("offline".into())).unwrap();
        let new = read(&t.0).unwrap().unwrap();
        assert_eq!(old.fetched_at, new.fetched_at);
        assert_eq!(old.report, new.report);
        assert_eq!(new.error.as_deref(), Some("offline"));
    }
    #[test]
    fn late_reply_cannot_overwrite_a_changed_source_or_device() {
        for device in [true, false] {
            let t = Temp::new();
            setup(&t.0);
            refresh_with(&t.0, |_| {
                let mut s = load(&t.0).unwrap().unwrap();
                if device {
                    s.device = "b".repeat(32);
                } else {
                    s.source = Source::Ssh {
                        target: "other".into(),
                    };
                }
                save(&t.0, &s).unwrap();
                Ok(report())
            })
            .unwrap();
            assert!(read(&t.0).unwrap().is_none());
        }
    }
    #[test]
    fn cache_and_reports_are_bounded_and_old_hosts_are_explicit() {
        let t = Temp::new();
        std::fs::write(t.0.join(FILE), vec![b' '; LIMIT as usize + 1]).unwrap();
        assert!(read(&t.0).is_err());
        assert!(
            Report::from_value(serde_json::json!({"restricted_sync_v1":true}))
                .unwrap_err()
                .contains("Update")
        );
        let mut r = report();
        r.managers = vec![Manager {
            address: "127.0.0.1".parse().unwrap(),
            last_exchange: 1000,
            in_sync: true,
            authentication: "restricted_key".into(),
            key_fingerprint: None,
        }];
        assert!(r.validate().is_err());
    }
    #[test]
    fn unpublished_catalog_edits_invalidate_a_current_observation() {
        let t = Temp::new();
        setup(&t.0);
        assert!(!snapshot_at(&t.0).unwrap().pending);
        let mut catalog = EndpointCatalog::default();
        catalog.add_ssh("Build", "build", "main").unwrap();
        super::super::super::store_private_json(
            &t.0.join("endpoints.json"),
            &serde_json::to_vec(&catalog).unwrap(),
            "test",
        )
        .unwrap();
        assert!(snapshot_at(&t.0).unwrap().pending);
    }
}
