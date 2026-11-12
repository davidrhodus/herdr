//! Peer synchronization runs on its own worker, never in view/render/PTY paths.
//! A private local journal preserves offline edits and a shared file lock fences
//! simultaneous CLI mutations, UI workers, peer changes, and network replies.
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use herdr_sync::{Document, Machine, Record, Stamp, MAX_BODY};
use serde::{Deserialize, Serialize};

use super::{EndpointCatalog, ProfileId, SavedSshEndpoint};
mod automatic;
pub(crate) use automatic::enable as enable_automatic;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Source {
    Local,
    Direct,
    Ssh { target: String },
}
impl Source {
    fn validate(&self) -> Result<(), String> {
        if let Self::Ssh { target } = self {
            if target.len() > 1024 || target.chars().any(char::is_control) {
                return Err("invalid sync SSH target".into());
            }
            crate::remote::validate_remote_target(target)?;
            let authority = target.strip_prefix("ssh://").unwrap_or(target);
            if authority
                .rsplit_once('@')
                .is_some_and(|(user, _)| user.contains(':'))
            {
                return Err("sync SSH target must not contain a password".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncState {
    version: u32,
    source: Source,
    device: String,
    document: Document,
    baseline: Vec<SavedSshEndpoint>,
    #[serde(default)]
    pending_projection: Option<Vec<SavedSshEndpoint>>,
    last_success: Option<u64>,
    last_error: Option<String>,
}

fn directory() -> PathBuf {
    crate::config::state_dir().join("client")
}
fn state_path(root: &Path) -> PathBuf {
    root.join("catalog-sync.json")
}

fn lock(root: &Path) -> Result<File, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let path = root.join("catalog.lock");
    let file = match crate::platform::create_private_state_file(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(&path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("catalog lock must not be a symlink".into());
            }
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| e.to_string())?
        }
        Err(error) => return Err(format!("could not create catalog lock: {error}")),
    };
    file.lock()
        .map_err(|e| format!("could not lock catalog: {e}"))?;
    Ok(file)
}
fn load(root: &Path) -> Result<Option<SyncState>, String> {
    let file = match File::open(state_path(root)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read peer sync state: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BODY as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BODY {
        return Err("peer sync state exceeds size limit".into());
    }
    let state: SyncState = serde_json::from_slice(&bytes).map_err(|_| "invalid peer sync state")?;
    if state.version != 1 || !herdr_sync::valid_id(&state.device) {
        return Err("invalid peer sync state version or device".into());
    }
    state.source.validate()?;
    state.document.validate()?;
    Ok(Some(state))
}
fn save(root: &Path, state: &SyncState) -> Result<(), String> {
    state.document.validate()?;
    let bytes = serde_json::to_vec(state).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BODY {
        return Err("peer sync state exceeds size limit".into());
    }
    let temp = root.join(format!(".catalog-sync-{}.tmp", ProfileId::generate()));
    let mut file = crate::platform::create_private_state_file(&temp).map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        crate::platform::replace_file(&temp, &state_path(root)).map_err(|e| e.to_string())?;
        crate::platform::sync_parent_directory(root).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
fn recover(root: &Path, state: &mut SyncState) -> Result<(), String> {
    let Some(projection) = state.pending_projection.as_ref() else {
        return Ok(());
    };
    let mut catalog = EndpointCatalog::load_from_path(&root.join("endpoints.json"))?;
    catalog.ssh = projection.clone();
    if catalog
        .selected_profile
        .as_ref()
        .is_some_and(|id| !catalog.ssh.iter().any(|p| &p.id == id && p.enabled))
    {
        catalog.selected_profile = None;
    }
    catalog.store_to_path(&root.join("endpoints.json"))?;
    state.pending_projection = None;
    save(root, state)
}
fn machine(profile: &SavedSshEndpoint) -> Machine {
    Machine {
        id: profile.id.to_string(),
        label: profile.label.clone(),
        target: profile.target.clone(),
        session: profile.session.clone(),
        enabled: profile.enabled,
    }
}
fn profiles(document: &Document) -> Result<Vec<SavedSshEndpoint>, String> {
    document
        .machines()
        .map(|value| {
            let mut profile = SavedSshEndpoint::new(&value.label, &value.target, &value.session)?;
            profile.id = ProfileId::parse(&value.id)?;
            profile.enabled = value.enabled;
            Ok(profile)
        })
        .collect()
}
impl SyncState {
    fn stage(&mut self, current: &[SavedSshEndpoint]) -> Result<(), String> {
        if self.baseline == current {
            return Ok(());
        }
        let mut changes = Vec::new();
        for before in &self.baseline {
            if !current.iter().any(|p| p.id == before.id) {
                changes.push((before.id.to_string(), None));
            }
        }
        for after in current {
            if !self.baseline.iter().any(|p| p == after) {
                changes.push((after.id.to_string(), Some(machine(after))));
            }
        }
        let mut document = self.document.clone();
        let mut clock = document.clock();
        for (id, machine) in changes {
            clock = clock
                .checked_add(1)
                .ok_or("state logical clock exhausted")?;
            let record = Record {
                id,
                machine,
                stamp: Stamp {
                    counter: clock,
                    device: self.device.clone(),
                },
            };
            document = document.merge(&Document {
                records: vec![record],
                ..Document::default()
            })?;
        }
        self.document = document;
        self.baseline = current.to_vec();
        Ok(())
    }
}

/// Apply only changes since this CLI loaded the catalog. Independent additions
/// from another CLI or a sync reply must survive a stale load/modify/store cycle.
pub(super) fn store_profiles(catalog: &EndpointCatalog) -> Result<(), String> {
    store_profiles_at(&directory(), catalog)
}
fn store_profiles_at(root: &Path, desired: &EndpointCatalog) -> Result<(), String> {
    let _guard = lock(root)?;
    desired.validate()?;
    match load(root) {
        Ok(Some(mut state)) => recover(root, &mut state)?,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, "invalid state journal; saving machine profiles locally")
        }
    }
    let path = root.join("endpoints.json");
    let mut current = EndpointCatalog::load_from_path(&path)?;
    if let Some(baseline) = &desired.baseline {
        for before in baseline {
            if !desired.ssh.iter().any(|p| p.id == before.id) {
                current.remove_ssh(&before.id);
            }
        }
        for after in &desired.ssh {
            if !baseline.iter().any(|p| p == after) {
                if let Some(existing) = current.ssh.iter_mut().find(|p| p.id == after.id) {
                    *existing = after.clone();
                } else if !baseline.iter().any(|p| p.id == after.id) {
                    current.ssh.push(after.clone());
                }
                // Do not resurrect a profile concurrently removed by sync.
            }
        }
    } else {
        for profile in &desired.ssh {
            if let Some(existing) = current.ssh.iter_mut().find(|p| p.id == profile.id) {
                *existing = profile.clone();
            } else {
                current.ssh.push(profile.clone());
            }
        }
    }
    if current
        .selected_profile
        .as_ref()
        .is_some_and(|id| !current.ssh.iter().any(|p| &p.id == id && p.enabled))
    {
        current.selected_profile = None;
    }
    current.store_to_path(&path)
}

pub(crate) fn sync_once() -> Result<bool, String> {
    sync_at(&directory())
}
fn sync_at(root: &Path) -> Result<bool, String> {
    let source = {
        let _guard = lock(root)?;
        load(root)?.map(|s| s.source)
    };
    if source == Some(Source::Direct) {
        return Ok(true);
    }
    let is_host = source == Some(Source::Local);
    if is_host {
        host_exchange(root, &Document::default())?;
        return Ok(true);
    }
    sync_with(root, |source, document| match source {
        Source::Ssh { target } => {
            crate::remote::exchange_catalog(target, document).map_err(|e| e.to_string())
        }
        Source::Local => Err("peer source changed during synchronization; retry".into()),
        Source::Direct => Ok(document.clone()),
    })
}
fn sync_with(
    root: &Path,
    exchange: impl FnOnce(&Source, &Document) -> Result<Document, String>,
) -> Result<bool, String> {
    let snapshot = {
        let _guard = lock(root)?;
        let Some(mut state) = load(root)? else {
            return Ok(false);
        };
        recover(root, &mut state)?;
        let current = EndpointCatalog::load_from_path(&root.join("endpoints.json"))?;
        let changed = state.baseline != current.ssh;
        state.stage(&current.ssh)?;
        if changed {
            save(root, &state)?;
        } // Durable outbox before attempting the network.
        state
    };
    let reply = exchange(&snapshot.source, &snapshot.document);
    let _guard = lock(root)?;
    let Some(mut state) = load(root)? else {
        return Ok(false);
    };
    if state.device != snapshot.device || state.source != snapshot.source {
        return Ok(false);
    }
    recover(root, &mut state)?;
    let catalog = EndpointCatalog::load_from_path(&root.join("endpoints.json"))?;
    state.stage(&catalog.ssh)?; // Include changes made while SSH was in flight.
    match reply {
        Ok(document) => {
            let merged = state.document.merge(&document)?;
            publish(root, &mut state, merged)?;
            background_exchange(root, &state.source, None);
            Ok(true)
        }
        Err(error) => {
            let error: String = error.chars().take(2048).collect();
            state.last_error = Some(error.clone());
            save(root, &state)?;
            background_exchange(root, &state.source, Some(&error));
            Err(error)
        }
    }
}

fn background_exchange(root: &Path, source: &Source, error: Option<&str>) {
    if std::env::var_os("HERDR_SYNC_SERVICE").is_none() || !matches!(source, Source::Ssh { .. }) {
        return;
    }
    let value = serde_json::json!({"pid":std::process::id(),"at":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),"ok":error.is_none(),"error":error,"source":source});
    let _ = super::store_private_json(
        &root.join("catalog-network-status.json"),
        &serde_json::to_vec(&value).unwrap_or_default(),
        "background network status",
    );
}

fn publish(root: &Path, state: &mut SyncState, merged: Document) -> Result<(), String> {
    let incoming = profiles(&merged)?;
    let mut catalog = EndpointCatalog::load_from_path(&root.join("endpoints.json"))?;
    let changed = catalog.ssh != incoming;
    catalog.ssh = incoming.clone();
    if catalog
        .selected_profile
        .as_ref()
        .is_some_and(|id| !catalog.ssh.iter().any(|p| &p.id == id && p.enabled))
    {
        catalog.selected_profile = None;
    }
    catalog.validate()?;
    if serde_json::to_vec_pretty(&catalog)
        .map_err(|e| e.to_string())?
        .len()
        > herdr_sync::MAX_CATALOG_BYTES
    {
        return Err("synced catalog exceeds local storage limit".into());
    }
    state.baseline = incoming.clone();
    state.pending_projection = changed.then_some(incoming);
    state.document = merged;
    state.last_success = Some(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    );
    state.last_error = None;
    save(root, state)?;
    if changed {
        recover(root, state)
    } else {
        Ok(())
    }
}

pub(crate) fn start_worker(quit: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut previous_error = None;
        let mut lease = None;
        let mut automatic_due = std::time::Instant::now();
        while !quit.load(Ordering::Acquire) {
            let root = directory();
            let resident = std::env::var_os("HERDR_SYNC_SERVICE").is_none()
                && crate::client::sync_background::status()["running"] == true;
            if resident {
                lease = None;
            }
            if lease.is_none() && !resident {
                if let Ok(file) =
                    crate::client::direct_sync::worker_lease(&root, "catalog-sync-owner.lock")
                {
                    if file.try_lock().is_ok() {
                        lease = Some(file);
                    }
                }
            }
            if lease.is_some() && std::time::Instant::now() >= automatic_due {
                automatic_due = std::time::Instant::now() + Duration::from_secs(60);
                if let Err(error) = automatic::enroll(&root) {
                    tracing::warn!(%error, "automatic server-list discovery paused");
                }
            }
            if lease.is_some() && state_path(&root).exists() {
                match sync_once() {
                    Err(error) => {
                        if previous_error.as_ref() != Some(&error) {
                            tracing::warn!(%error, "peer sync paused; local machine profiles retained");
                        }
                        previous_error = Some(error);
                    }
                    Ok(_) => previous_error = None,
                }
            }
            for _ in 0..15 {
                if quit.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });
}

pub(crate) fn configure(source: Source) -> Result<(), String> {
    source.validate()?;
    let root = directory();
    {
        let _guard = lock(&root)?;
        let mut state = match load(&root)? {
            Some(mut state) => {
                if state.source != source {
                    return Err(
                        "run `herdr sync leave` before changing the shared-list peer".into(),
                    );
                }
                recover(&root, &mut state)?;
                state
            }
            None => SyncState {
                version: 1,
                source,
                device: ProfileId::generate().to_string(),
                document: Document::default(),
                baseline: Vec::new(),
                pending_projection: None,
                last_success: None,
                last_error: None,
            },
        };
        automatic::set_disabled(&root, false)?;
        state.stage(&EndpointCatalog::load_from_path(&root.join("endpoints.json"))?.ssh)?;
        save(&root, &state)?;
    }
    sync_once()?;
    Ok(())
}

fn host_exchange(root: &Path, incoming: &Document) -> Result<Document, String> {
    incoming.validate()?;
    let _guard = lock(root)?;
    let Some(mut state) = load(root)? else {
        return Err("this peer is not hosting a shared list; run `herdr sync host` on it".into());
    };
    if state.source != Source::Local {
        return Err(
            "this peer is not a shared-list host; choose its configured source instead".into(),
        );
    }
    recover(root, &mut state)?;
    state.stage(&EndpointCatalog::load_from_path(&root.join("endpoints.json"))?.ssh)?;
    let merged = state.document.merge(incoming)?;
    publish(root, &mut state, merged.clone())?;
    Ok(merged)
}

// Direct transport holds its pairing lock before entering these catalog transactions.
// The journal device changes on transport switches, fencing all in-flight replies.
fn revision(state: Option<&SyncState>) -> Result<String, String> {
    state.map_or(Ok("off".into()), |state| {
        Ok(format!(
            "{}:{}",
            state.device,
            serde_json::to_string(&state.source).map_err(|e| e.to_string())?
        ))
    })
}
pub(crate) fn direct_revision_at(root: &Path) -> Result<String, String> {
    let _guard = lock(root)?;
    revision(load(root)?.as_ref())
}
#[cfg(test)]
pub(crate) fn enable_direct_at(root: &Path) -> Result<(), String> {
    enable_direct_when(root, None, &ProfileId::generate().to_string()).map(|_| ())
}
pub(crate) fn enable_direct_when(
    root: &Path,
    expected: Option<&str>,
    promotion: &str,
) -> Result<String, String> {
    ProfileId::parse(promotion)?;
    let _guard = lock(root)?;
    let previous = load(root)?;
    if let Some(expected) = expected {
        if revision(previous.as_ref())? != expected
            && !previous
                .as_ref()
                .is_some_and(|state| state.source == Source::Direct && state.device == promotion)
        {
            return Err("Sync configuration changed. Start pairing again.".into());
        }
    }
    let mut state = match previous {
        Some(mut state) => {
            recover(root, &mut state)?;
            if state.source != Source::Direct {
                state.source = Source::Direct;
                state.device = promotion.into();
            }
            state
        }
        None => SyncState {
            version: 1,
            source: Source::Direct,
            device: promotion.into(),
            document: Document::default(),
            baseline: vec![],
            pending_projection: None,
            last_success: None,
            last_error: None,
        },
    };
    state.stage(&EndpointCatalog::load_from_path(&root.join("endpoints.json"))?.ssh)?;
    automatic::set_disabled(root, false)?;
    save(root, &state)?;
    Ok(state.device)
}
pub(crate) fn direct_snapshot_at(root: &Path) -> Result<(String, Document), String> {
    let _guard = lock(root)?;
    let mut state = load(root)?.ok_or("Direct sync is disabled")?;
    if state.source != Source::Direct {
        return Err("Direct sync is disabled".into());
    }
    recover(root, &mut state)?;
    state.stage(&EndpointCatalog::load_from_path(&root.join("endpoints.json"))?.ssh)?;
    save(root, &state)?;
    Ok((state.device, state.document))
}
pub(crate) fn direct_merge_at(
    root: &Path,
    epoch: &str,
    incoming: &Document,
) -> Result<Document, String> {
    incoming.validate()?;
    let _guard = lock(root)?;
    let mut state = load(root)?.ok_or("Direct sync was disabled")?;
    if state.source != Source::Direct || state.device != epoch {
        return Err("Sync configuration changed; retry".into());
    }
    recover(root, &mut state)?;
    state.stage(&EndpointCatalog::load_from_path(&root.join("endpoints.json"))?.ssh)?;
    let merged = state.document.merge(incoming)?;
    publish(root, &mut state, merged.clone())?;
    Ok(merged)
}

pub(crate) fn leave_direct_at(root: &Path) -> Result<(), String> {
    let _guard = lock(root)?;
    automatic::set_disabled(root, true)?;
    if let Some(mut state) = load(root)? {
        if state.source == Source::Direct {
            recover(root, &mut state)?;
            std::fs::remove_file(state_path(root)).map_err(|e| e.to_string())?;
            crate::platform::sync_parent_directory(root).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn exchange_stdio() -> Result<(), String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BODY {
        return Err("catalog sync input exceeds size limit".into());
    }
    let incoming: Document =
        serde_json::from_slice(&bytes).map_err(|_| "invalid catalog sync request")?;
    let result = host_exchange(&directory(), &incoming)?;
    serde_json::to_writer(std::io::stdout().lock(), &result).map_err(|e| e.to_string())
}

pub(crate) fn status() -> Result<serde_json::Value, String> {
    let _guard = lock(&directory())?;
    let mut value = match load(&directory())? {
        None => serde_json::json!({"enabled": false}),
        Some(state) => {
            serde_json::json!({"enabled": true, "source": state.source, "last_success": state.last_success, "last_error": state.last_error, "machines": state.document.machines().count()})
        }
    };
    if let Some(state) = load(&directory())? {
        if state.source == Source::Local {
            value["registry_id"] = serde_json::Value::String(state.device);
        }
    }
    value["automatic"] = serde_json::Value::Bool(!automatic::disabled(&directory()));
    Ok(value)
}
pub(crate) fn leave() -> Result<(), String> {
    leave_at(&directory())
}
pub(crate) fn leave_at(root: &Path) -> Result<(), String> {
    let _guard = lock(root)?;
    automatic::set_disabled(root, true)?;
    match load(root) {
        Ok(Some(mut state)) => recover(root, &mut state)?,
        Ok(None) => {}
        Err(error) => eprintln!("Removing invalid peer sync state: {error}"),
    }
    match std::fs::remove_file(state_path(root)) {
        Ok(()) => crate::platform::sync_parent_directory(root).map_err(|e| e.to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    pub(super) struct Temp(pub(super) PathBuf);
    impl Temp {
        pub(super) fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("herdr-catalog-sync-{}", ProfileId::generate()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn catalog(&self) -> EndpointCatalog {
            EndpointCatalog::load_from_path(&self.0.join("endpoints.json")).unwrap()
        }
        fn join(&self, device: char) {
            let state = SyncState {
                version: 1,
                source: Source::Ssh {
                    target: "desktop".into(),
                },
                device: device.to_string().repeat(32),
                document: Document::default(),
                baseline: Vec::new(),
                pending_projection: None,
                last_success: None,
                last_error: None,
            };
            save(&self.0, &state).unwrap();
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn add(temp: &Temp, label: &str) -> ProfileId {
        let mut catalog = temp.catalog();
        let id = catalog.add_ssh(label, "build", "default").unwrap();
        store_profiles_at(&temp.0, &catalog).unwrap();
        id
    }
    fn exchange(server: &Mutex<Document>, document: &Document) -> Result<Document, String> {
        let mut shared = server.lock().unwrap();
        *shared = shared.merge(document)?;
        Ok(shared.clone())
    }
    fn round(temp: &Temp, server: &Mutex<Document>) {
        assert!(sync_with(&temp.0, |_, document| exchange(server, document)).unwrap());
    }
    #[test]
    fn file_backed_host_and_laptop_converge_without_a_running_host_ui() {
        let host = Temp::new();
        let laptop = Temp::new();
        let first = add(&host, "Host server");
        let second = add(&laptop, "Laptop server");
        host.join('a');
        laptop.join('b');
        let mut state = load(&host.0).unwrap().unwrap();
        state.source = Source::Local;
        save(&host.0, &state).unwrap();
        sync_with(&laptop.0, |_, doc| host_exchange(&host.0, doc)).unwrap();
        assert_eq!(host.catalog().ssh, laptop.catalog().ssh);
        assert_eq!(host.catalog().ssh.len(), 2);
        let mut renamed = host.catalog();
        renamed.rename_ssh(&first, "Host rename").unwrap();
        store_profiles_at(&host.0, &renamed).unwrap();
        let mut removed = laptop.catalog();
        removed.remove_ssh(&second);
        store_profiles_at(&laptop.0, &removed).unwrap();
        sync_with(&laptop.0, |_, doc| host_exchange(&host.0, doc)).unwrap();
        assert_eq!(host.catalog().ssh, laptop.catalog().ssh);
        assert_eq!(host.catalog().ssh.len(), 1);
        assert_eq!(laptop.catalog().ssh[0].label, "Host rename");
    }
    #[test]
    fn unconfigured_and_non_host_peers_refuse_exchanges_without_catalog_changes() {
        let peer = Temp::new();
        add(&peer, "Private local list");
        assert!(host_exchange(&peer.0, &Document::default()).is_err());
        assert_eq!(peer.catalog().ssh.len(), 1);
        peer.join('a');
        assert!(host_exchange(&peer.0, &Document::default()).is_err());
        assert_eq!(peer.catalog().ssh.len(), 1);
    }
    #[test]
    fn lost_initial_reply_cannot_discard_an_offline_rename_of_an_identical_profile() {
        let desktop = Temp::new();
        let laptop = Temp::new();
        add(&desktop, "Redie");
        add(&laptop, "Redie");
        desktop.join('a');
        laptop.join('b');
        let server = Mutex::new(Document::default());
        round(&desktop, &server);
        assert!(sync_with(&laptop.0, |_, document| {
            let mut hosted = server.lock().unwrap();
            *hosted = hosted.merge(document)?;
            Err("reply lost".into())
        })
        .is_err());
        let mut catalog = laptop.catalog();
        catalog.ssh[0].label = "Production".into();
        catalog
            .store_to_path(&laptop.0.join("endpoints.json"))
            .unwrap();
        round(&laptop, &server);
        round(&desktop, &server);
        assert_eq!(desktop.catalog().ssh, laptop.catalog().ssh);
        assert_eq!(desktop.catalog().ssh.len(), 2);
        assert!(desktop
            .catalog()
            .ssh
            .iter()
            .any(|p| p.label == "Production"));
    }
    #[test]
    fn two_devices_join_with_existing_profiles_and_sync_offline_adds_renames_and_deletes() {
        let desktop = Temp::new();
        let laptop = Temp::new();
        let first = add(&desktop, "Desktop");
        let second = add(&laptop, "Laptop");
        desktop.join('a');
        laptop.join('b');
        let server = Mutex::new(Document::default());
        round(&desktop, &server);
        round(&laptop, &server);
        round(&desktop, &server);
        assert_eq!(desktop.catalog().ssh, laptop.catalog().ssh);
        assert_eq!(desktop.catalog().ssh.len(), 2); // Duplicate destination is allowed.
        assert!(desktop.catalog().ssh.iter().any(|p| p.id == first));
        assert!(desktop.catalog().ssh.iter().any(|p| p.id == second));
        let mut offline = laptop.catalog();
        offline.rename_ssh(&first, "Offline rename").unwrap();
        store_profiles_at(&laptop.0, &offline).unwrap();
        assert!(sync_with(&laptop.0, |_, _| Err("offline".into())).is_err());
        assert!(load(&laptop.0).unwrap().unwrap().last_error.is_some());
        let mut removed = desktop.catalog();
        removed.remove_ssh(&first);
        store_profiles_at(&desktop.0, &removed).unwrap();
        round(&desktop, &server);
        round(&laptop, &server);
        round(&desktop, &server);
        assert_eq!(laptop.catalog().ssh, desktop.catalog().ssh);
        assert!(!laptop.catalog().ssh.iter().any(|p| p.id == first));
        let replacement = add(&laptop, "Re-added");
        assert_ne!(replacement, first);
        round(&laptop, &server);
        round(&desktop, &server);
        assert!(desktop.catalog().ssh.iter().any(|p| p.id == replacement));
    }
    #[test]
    fn stale_cli_snapshots_keep_independent_changes_and_never_resurrect_removals() {
        let temp = Temp::new();
        let id = add(&temp, "Original");
        let mut a = temp.catalog();
        let mut b = temp.catalog();
        a.add_ssh("New", "new", "default").unwrap();
        b.rename_ssh(&id, "Renamed").unwrap();
        store_profiles_at(&temp.0, &a).unwrap();
        store_profiles_at(&temp.0, &b).unwrap();
        assert_eq!(temp.catalog().ssh.len(), 2);
        assert_eq!(
            temp.catalog()
                .ssh
                .iter()
                .find(|p| p.id == id)
                .unwrap()
                .label,
            "Renamed"
        );
        let mut deleted = temp.catalog();
        deleted.remove_ssh(&id);
        store_profiles_at(&temp.0, &deleted).unwrap();
        store_profiles_at(&temp.0, &b).unwrap();
        assert!(!temp.catalog().ssh.iter().any(|p| p.id == id));
    }
    #[test]
    fn edit_during_ssh_request_is_merged_then_uploaded_on_next_round() {
        let temp = Temp::new();
        let id = add(&temp, "Before");
        temp.join('a');
        let server = Mutex::new(Document::default());
        sync_with(&temp.0, |_, document| {
            let mut during = temp.catalog();
            during.rename_ssh(&id, "During").unwrap();
            store_profiles_at(&temp.0, &during).unwrap();
            exchange(&server, document)
        })
        .unwrap();
        assert_eq!(temp.catalog().ssh[0].label, "During");
        round(&temp, &server);
        assert_eq!(
            server.lock().unwrap().machines().next().unwrap().label,
            "During"
        );
    }
    #[test]
    fn peer_changes_fence_inflight_replies_and_invalid_responses_keep_local_catalog() {
        let temp = Temp::new();
        add(&temp, "Local");
        temp.join('a');
        assert!(!sync_with(&temp.0, |_, _| {
            std::fs::remove_file(state_path(&temp.0)).unwrap();
            Ok(Document::default())
        })
        .unwrap());
        assert_eq!(temp.catalog().ssh.len(), 1);
        temp.join('a');
        assert!(sync_with(&temp.0, |_, _| Ok(Document {
            version: 999,
            records: Vec::new()
        }))
        .is_err());
        assert_eq!(temp.catalog().ssh.len(), 1);
        let state = status_for_test(&temp);
        assert!(!state.contains("selected_profile"));
    }
    fn status_for_test(temp: &Temp) -> String {
        serde_json::to_string(&load(&temp.0).unwrap().unwrap().document).unwrap()
    }
    #[test]
    fn interrupted_projection_publication_recovers_without_resurrection_or_lost_profiles() {
        let desktop = Temp::new();
        let laptop = Temp::new();
        add(&desktop, "Remote");
        desktop.join('a');
        laptop.join('b');
        let server = Mutex::new(Document::default());
        round(&desktop, &server);
        // Simulate projection write succeeding and the process dying before the
        // journal baseline and merged document were persisted.
        let mut projection = laptop.catalog();
        projection.ssh = profiles(&server.lock().unwrap()).unwrap();
        projection
            .store_to_path(&laptop.0.join("endpoints.json"))
            .unwrap();
        round(&laptop, &server);
        round(&desktop, &server);
        assert_eq!(laptop.catalog().ssh, desktop.catalog().ssh);
        assert_eq!(laptop.catalog().ssh.len(), 1);
    }
    #[test]
    fn file_lock_serializes_concurrent_cli_writers() {
        let temp = Temp::new();
        let mut a = temp.catalog();
        let mut b = temp.catalog();
        a.add_ssh("A", "a", "default").unwrap();
        b.add_ssh("B", "b", "default").unwrap();
        let root = temp.0.clone();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| store_profiles_at(&root, &a));
            let second = scope.spawn(|| store_profiles_at(&root, &b));
            first.join().unwrap().unwrap();
            second.join().unwrap().unwrap();
        });
        assert_eq!(temp.catalog().ssh.len(), 2);
    }
}

#[cfg(test)]
mod crash_tests {
    use super::*;
    #[test]
    fn pending_projection_is_recovered_before_a_remote_value_can_be_restamped() {
        let root = std::env::temp_dir().join(format!("herdr-sync-crash-{}", ProfileId::generate()));
        std::fs::create_dir_all(&root).unwrap();
        let mut catalog = EndpointCatalog::default();
        let id = catalog.add_ssh("Old", "host", "default").unwrap();
        let profile = catalog.ssh[0].clone();
        let remote = Document {
            records: vec![Record {
                id: id.to_string(),
                machine: Some(machine(&profile)),
                stamp: Stamp {
                    counter: 1,
                    device: "a".repeat(32),
                },
            }],
            ..Document::default()
        };
        let mut state = SyncState {
            version: 1,
            source: Source::Ssh {
                target: "desktop".into(),
            },
            device: "f".repeat(32),
            document: remote.clone(),
            baseline: vec![profile.clone()],
            pending_projection: Some(vec![profile]),
            last_success: None,
            last_error: None,
        };
        // Intent is durable; projection has not yet been published (or may have
        // been published just before a crash). A newer remote rename then occurs.
        save(&root, &state).unwrap();
        catalog.store_to_path(&root.join("endpoints.json")).unwrap();
        let mut renamed = remote.clone();
        renamed.records[0].stamp.counter = 2;
        renamed.records[0].machine.as_mut().unwrap().label = "New".into();
        sync_with(&root, |_, incoming| {
            assert_eq!(incoming.records[0].stamp, remote.records[0].stamp);
            incoming.merge(&renamed)
        })
        .unwrap();
        assert_eq!(
            EndpointCatalog::load_from_path(&root.join("endpoints.json"))
                .unwrap()
                .ssh[0]
                .label,
            "New"
        );
        state = load(&root).unwrap().unwrap();
        assert!(state.pending_projection.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn disabling_a_legacy_selected_profile_clears_its_legacy_selection() {
        let root =
            std::env::temp_dir().join(format!("herdr-sync-legacy-{}", ProfileId::generate()));
        std::fs::create_dir_all(&root).unwrap();
        let mut catalog = EndpointCatalog::default();
        let id = catalog.add_ssh("Selected", "host", "default").unwrap();
        catalog.select_ssh(&id);
        catalog.store_to_path(&root.join("endpoints.json")).unwrap();
        let mut desired = EndpointCatalog::load_from_path(&root.join("endpoints.json")).unwrap();
        desired.set_enabled(&id, false);
        store_profiles_at(&root, &desired).unwrap();
        let actual = EndpointCatalog::load_from_path(&root.join("endpoints.json")).unwrap();
        assert!(!actual.ssh[0].enabled);
        assert!(actual.selected_profile.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod inflight_recovery_tests {
    use super::*;
    #[test]
    fn reply_path_recovers_another_workers_pending_projection_before_staging() {
        let root =
            std::env::temp_dir().join(format!("herdr-sync-inflight-{}", ProfileId::generate()));
        std::fs::create_dir_all(&root).unwrap();
        let mut state = SyncState {
            version: 1,
            source: Source::Ssh {
                target: "desktop".into(),
            },
            device: "a".repeat(32),
            document: Document::default(),
            baseline: Vec::new(),
            pending_projection: None,
            last_success: None,
            last_error: None,
        };
        save(&root, &state).unwrap();
        let profile = SavedSshEndpoint::new("Remote", "host", "default").unwrap();
        let remote = Document {
            records: vec![Record {
                id: profile.id.to_string(),
                machine: Some(machine(&profile)),
                stamp: Stamp {
                    counter: 1,
                    device: "b".repeat(32),
                },
            }],
            ..Document::default()
        };
        sync_with(&root, |_, _| {
            state.document = remote.clone();
            state.baseline = vec![profile.clone()];
            state.pending_projection = Some(vec![profile]);
            save(&root, &state).unwrap(); // Another worker persisted intent and died.
            Ok(remote.clone())
        })
        .unwrap();
        let recovered = load(&root).unwrap().unwrap();
        assert_eq!(recovered.document, remote);
        assert_eq!(
            EndpointCatalog::load_from_path(&root.join("endpoints.json"))
                .unwrap()
                .ssh
                .len(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    #[test]
    fn sixty_four_maximum_length_profiles_fit_the_native_catalog() {
        let mut catalog = EndpointCatalog::default();
        for n in 0..64 {
            let id = catalog
                .add_ssh("L".repeat(128), "h".repeat(1024), "s".repeat(64))
                .unwrap();
            // IDs remain independent even when all destinations are the same.
            assert_eq!(catalog.ssh[n].id, id);
        }
        catalog.validate().unwrap();
        let bytes = serde_json::to_vec_pretty(&catalog).unwrap();
        assert!(bytes.len() > 64 * 1024);
        assert!(bytes.len() <= herdr_sync::MAX_CATALOG_BYTES);
        let root =
            std::env::temp_dir().join(format!("herdr-sync-capacity-{}", ProfileId::generate()));
        catalog.store_to_path(&root.join("endpoints.json")).unwrap();
        assert_eq!(
            EndpointCatalog::load_from_path(&root.join("endpoints.json"))
                .unwrap()
                .ssh,
            catalog.ssh
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;
    #[test]
    fn sync_source_rejects_passwords_options_and_control_characters() {
        for target in [
            "ssh://user:secret@host",
            "user:secret@host",
            "-oProxyCommand=bad",
            "host\nother",
        ] {
            assert!(
                Source::Ssh {
                    target: target.into()
                }
                .validate()
                .is_err(),
                "{target}"
            );
        }
        for target in [
            "you@desktop",
            "desktop.tailnet.ts.net",
            "ssh://you@host:2222",
        ] {
            assert!(
                Source::Ssh {
                    target: target.into()
                }
                .validate()
                .is_ok(),
                "{target}"
            );
        }
    }
    #[test]
    fn replies_from_a_previous_source_or_device_cannot_modify_the_new_configuration() {
        for change_device in [false, true] {
            let root =
                std::env::temp_dir().join(format!("herdr-sync-fence-{}", ProfileId::generate()));
            std::fs::create_dir_all(&root).unwrap();
            let mut state = SyncState {
                version: 1,
                source: Source::Ssh {
                    target: "old-host".into(),
                },
                device: "a".repeat(32),
                document: Document::default(),
                baseline: Vec::new(),
                pending_projection: None,
                last_success: None,
                last_error: None,
            };
            save(&root, &state).unwrap();
            let applied = sync_with(&root, |_, _| {
                if change_device {
                    state.device = "b".repeat(32);
                } else {
                    state.source = Source::Ssh {
                        target: "new-host".into(),
                    };
                }
                save(&root, &state).unwrap();
                Ok(Document::default())
            })
            .unwrap();
            assert!(!applied);
            let after = load(&root).unwrap().unwrap();
            assert_eq!(after.source, state.source);
            assert_eq!(after.device, state.device);
            assert_eq!(after.last_success, None);
            assert!(!root.join("endpoints.json").exists());
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
