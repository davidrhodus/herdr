//! Discover an account that already opted into hosting. Never provision a host
//! or override explicit local sync/pairing decisions from a background probe.
use super::*;

const DISABLED: &str = "catalog-sync-disabled.json";
const TARGETS: &str = "authenticated-sync-targets.json";
const MAX_TARGETS: usize = 64;
const MAX_TARGET_BYTES: u64 = 128 * 1024;

// Only successful, user-requested SSH connections become candidates. A target
// still must explicitly host a registry before any catalog is sent to it.
pub(crate) fn remember_authenticated_target(target: &str) {
    if cfg!(test) || std::env::var_os("HERDR_SYNC_SERVICE").is_some() {
        return;
    }
    if let Err(error) = remember_at(&directory(), target) {
        tracing::debug!(%error, "Could not remember authenticated sync candidate");
    }
}
fn remembered(root: &Path) -> Result<Vec<String>, String> {
    let file = match File::open(root.join(TARGETS)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_TARGET_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_TARGET_BYTES {
        return Err("authenticated target history exceeds size limit".into());
    }
    let targets: Vec<String> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if targets.len() > MAX_TARGETS {
        return Err("too many authenticated sync candidates".into());
    }
    for target in &targets {
        SavedSshEndpoint::new("Authenticated SSH host", target, "default")?;
    }
    Ok(targets)
}
fn remember_at(root: &Path, target: &str) -> Result<(), String> {
    SavedSshEndpoint::new("Authenticated SSH host", target, "default")?;
    let _guard = lock(root)?;
    if disabled(root) {
        return Ok(());
    }
    let mut targets = remembered(root)?;
    if EndpointCatalog::load_from_path(&root.join("endpoints.json"))?
        .ssh
        .iter()
        .any(|p| p.target == target)
    {
        // Saved profiles supply their own candidates and removal fence. Do not
        // retain a second route around deleting/disabling that saved profile.
        let previous = targets.len();
        targets.retain(|t| t != target);
        if targets.len() != previous {
            super::super::store_private_json(
                &root.join(TARGETS),
                &serde_json::to_vec(&targets).map_err(|e| e.to_string())?,
                "authenticated sync candidates",
            )?;
        }
        return Ok(());
    }
    if targets.first().is_some_and(|t| t == target) {
        return Ok(());
    }
    targets.retain(|t| t != target);
    targets.insert(0, target.into());
    targets.truncate(MAX_TARGETS);
    let bytes = serde_json::to_vec(&targets).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_TARGET_BYTES {
        return Err("authenticated target history exceeds size limit".into());
    }
    super::super::store_private_json(&root.join(TARGETS), &bytes, "authenticated sync candidates")
}

pub(super) fn disabled(root: &Path) -> bool {
    root.join(DISABLED).exists()
}
pub(super) fn set_disabled(root: &Path, disabled: bool) -> Result<(), String> {
    if disabled {
        super::super::store_private_json(&root.join(DISABLED), b"true\n", "sync opt-out")
    } else {
        match std::fs::remove_file(root.join(DISABLED)) {
            Ok(()) => crate::platform::sync_parent_directory(root).map_err(|e| e.to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

pub(super) fn enroll(root: &Path) -> Result<bool, String> {
    enroll_with(root, crate::remote::catalog_host_identity)
}
fn enroll_with(
    root: &Path,
    probe: impl Fn(&str) -> io::Result<Option<String>> + Sync,
) -> Result<bool, String> {
    let (profiles, original_revision, saved_ids) = {
        let _guard = lock(root)?;
        let state = load(root)?;
        if disabled(root) || state.as_ref().is_some_and(|s| s.source != Source::Direct) {
            return Ok(false);
        }
        let original_revision = revision(state.as_ref())?;
        let mut profiles = EndpointCatalog::load_from_path(&root.join("endpoints.json"))?.ssh;
        let saved_ids: std::collections::HashSet<_> =
            profiles.iter().map(|p| p.id.clone()).collect();
        let mut history = remembered(root)?;
        let previous_len = history.len();
        history.retain(|target| !profiles.iter().any(|p| &p.target == target));
        if history.len() != previous_len {
            super::super::store_private_json(
                &root.join(TARGETS),
                &serde_json::to_vec(&history).map_err(|e| e.to_string())?,
                "authenticated sync candidates",
            )?;
        }
        for target in history {
            // A disabled saved profile is an explicit decision, including when
            // the same target previously worked via --remote.
            if !profiles.iter().any(|p| p.target == target) {
                profiles.push(SavedSshEndpoint::new(
                    "Authenticated SSH host",
                    target,
                    "default",
                )?);
            }
        }
        (profiles, original_revision, saved_ids)
    };
    let mut targets = Vec::new();
    if !crate::client::direct_sync::when_unpaired(root, || Ok(true))? {
        return Ok(false);
    }
    for profile in profiles.iter().filter(|p| p.enabled) {
        if !targets
            .iter()
            .any(|p: &&SavedSshEndpoint| p.target == profile.target)
        {
            targets.push(profile);
        }
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let hosts = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..targets.len().min(4) {
            let probe = &probe;
            let targets = &targets;
            let next = &next;
            let hosts = &hosts;
            scope.spawn(move || loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(profile) = targets.get(index) else {
                    break;
                };
                if let Ok(Some(registry)) = probe(&profile.target) {
                    hosts
                        .lock()
                        .expect("host discovery lock")
                        .push(((*profile).clone(), registry));
                }
            });
        }
    });
    let hosts = hosts.into_inner().map_err(|_| "Host discovery stopped")?;
    crate::client::direct_sync::when_unpaired(root, || {
        let _guard = lock(root)?;
        let previous = load(root)?;
        if revision(previous.as_ref())? != original_revision || disabled(root) {
            return Ok(false);
        }
        if hosts
            .iter()
            .map(|(_, id)| id)
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 1
        {
            return Err(
                "Multiple shared server lists found. Choose one in Servers → Server-list sync."
                    .into(),
            );
        }
        let Some((profile, _)) = hosts.first() else {
            return Ok(false);
        };
        let mut state = previous.unwrap_or_else(|| SyncState {
            version: 1,
            source: Source::Direct,
            device: ProfileId::generate().to_string(),
            document: Document::default(),
            baseline: Vec::new(),
            pending_projection: None,
            last_success: None,
            last_error: None,
        });
        recover(root, &mut state)?;
        let current = EndpointCatalog::load_from_path(&root.join("endpoints.json"))?;
        let saved = current
            .ssh
            .iter()
            .any(|p| p.id == profile.id && p.target == profile.target && p.enabled);
        let authenticated = !current.ssh.iter().any(|p| p.target == profile.target)
            && remembered(root)?.contains(&profile.target);
        let eligible = if saved_ids.contains(&profile.id) {
            saved
        } else {
            authenticated
        };
        if !eligible {
            return Ok(false);
        }
        // An orphan Direct journal has no peers or active pairing intent (the
        // outer Direct lock checks both). Preserve its complete merge history.
        state.stage(&current.ssh)?;
        state.source = Source::Ssh {
            target: profile.target.clone(),
        };
        state.device = ProfileId::generate().to_string();
        state.last_success = None;
        state.last_error = None;
        save(root, &state)?;
        Ok(true)
    })
}

pub(crate) fn enable() -> Result<(), String> {
    let root = directory();
    let _guard = lock(&root)?;
    set_disabled(&root, false)
}

#[cfg(test)]
mod tests {
    use super::super::tests::Temp;
    use super::*;
    fn server(temp: &Temp, name: &str) -> SavedSshEndpoint {
        let mut catalog = EndpointCatalog::load_from_path(&temp.0.join("endpoints.json")).unwrap();
        let id = catalog
            .add_ssh(name, format!("david@{name}"), "default")
            .unwrap();
        let result = catalog.ssh.iter().find(|p| p.id == id).unwrap().clone();
        catalog
            .store_to_path(&temp.0.join("endpoints.json"))
            .unwrap();
        result
    }
    #[test]
    fn automatic_enrollment_requires_opted_in_host_and_preserves_explicit_opt_out() {
        let temp = Temp::new();
        let profile = server(&temp, "redie");
        assert!(!enroll_with(&temp.0, |_| Ok(None)).unwrap());
        assert!(load(&temp.0).unwrap().is_none());
        assert!(enroll_with(&temp.0, |_| Ok(Some("a".repeat(32)))).unwrap());
        assert_eq!(
            load(&temp.0).unwrap().unwrap().source,
            Source::Ssh {
                target: profile.target
            }
        );
        leave_at(&temp.0).unwrap();
        server(&temp, "later");
        assert!(!enroll_with(&temp.0, |_| Ok(Some("a".repeat(32)))).unwrap());
        assert!(load(&temp.0).unwrap().is_none());
    }
    #[test]
    fn ambiguous_hosts_and_disable_during_probe_cannot_choose_a_source() {
        let temp = Temp::new();
        server(&temp, "redie");
        server(&temp, "other");
        assert!(enroll_with(&temp.0, |target| Ok(Some(
            if target.ends_with("redie") { "a" } else { "b" }.repeat(32)
        )))
        .is_err());
        assert!(load(&temp.0).unwrap().is_none());
        assert!(!enroll_with(&temp.0, |_| {
            leave_at(&temp.0).unwrap();
            Ok(Some("a".repeat(32)))
        })
        .unwrap());
    }
    #[test]
    fn aliases_to_one_registry_are_not_mistaken_for_two_authorities() {
        let temp = Temp::new();
        server(&temp, "redie");
        server(&temp, "redie-alias");
        assert!(enroll_with(&temp.0, |_| Ok(Some("a".repeat(32)))).unwrap());
        assert!(matches!(
            load(&temp.0).unwrap().unwrap().source,
            Source::Ssh { .. }
        ));
    }
    #[test]
    fn manual_source_and_offline_authentication_remain_untouched() {
        let temp = Temp::new();
        server(&temp, "redie");
        assert!(!enroll_with(&temp.0, |_| Err(io::Error::other(
            "authentication required"
        )))
        .unwrap());
        assert!(enroll_with(&temp.0, |_| Ok(Some("a".repeat(32)))).unwrap());
        let before = std::fs::read(state_path(&temp.0)).unwrap();
        assert!(!enroll_with(&temp.0, |_| panic!("manual source must not be probed")).unwrap());
        assert_eq!(before, std::fs::read(state_path(&temp.0)).unwrap());
    }

    #[test]
    fn authenticated_remote_only_client_joins_an_opted_in_host_without_a_saved_profile() {
        let temp = Temp::new();
        remember_at(&temp.0, "david@redie").unwrap();
        assert!(!enroll_with(&temp.0, |_| Ok(None)).unwrap());
        assert!(load(&temp.0).unwrap().is_none());
        assert!(enroll_with(&temp.0, |target| {
            assert_eq!(target, "david@redie");
            Ok(Some("a".repeat(32)))
        })
        .unwrap());
        assert_eq!(
            load(&temp.0).unwrap().unwrap().source,
            Source::Ssh {
                target: "david@redie".into()
            }
        );
    }

    #[test]
    fn authenticated_history_does_not_bypass_disable_removal_or_inflight_changes() {
        let temp = Temp::new();
        remember_at(&temp.0, "david@redie").unwrap();
        assert!(!enroll_with(&temp.0, |_| {
            std::fs::remove_file(temp.0.join(TARGETS)).unwrap();
            Ok(Some("a".repeat(32)))
        })
        .unwrap());
        remember_at(&temp.0, "david@redie").unwrap();
        let profile = server(&temp, "redie");
        remember_at(&temp.0, &profile.target).unwrap();
        assert!(remembered(&temp.0).unwrap().is_empty());
        let mut catalog = EndpointCatalog::load_from_path(&temp.0.join("endpoints.json")).unwrap();
        catalog.ssh[0].enabled = false;
        catalog
            .store_to_path(&temp.0.join("endpoints.json"))
            .unwrap();
        assert!(!enroll_with(&temp.0, |_| panic!("disabled profiles must not be probed")).unwrap());
        catalog.ssh.clear();
        catalog
            .store_to_path(&temp.0.join("endpoints.json"))
            .unwrap();
        assert!(!enroll_with(&temp.0, |_| panic!("removed profiles must not be probed")).unwrap());
        leave_at(&temp.0).unwrap();
        remember_at(&temp.0, "david@other").unwrap();
        assert!(remembered(&temp.0).unwrap().is_empty());
    }

    #[test]
    fn history_is_bounded_deduplicated_and_validated() {
        let temp = Temp::new();
        for n in 0..70 {
            remember_at(&temp.0, &format!("david@host-{n}")).unwrap();
        }
        remember_at(&temp.0, "david@host-69").unwrap();
        assert_eq!(remembered(&temp.0).unwrap().len(), MAX_TARGETS);
        assert_eq!(remembered(&temp.0).unwrap()[0], "david@host-69");
        assert!(remember_at(&temp.0, "ssh://david:password@host").is_err());
        std::fs::write(
            temp.0.join(TARGETS),
            vec![b' '; MAX_TARGET_BYTES as usize + 1],
        )
        .unwrap();
        assert!(remembered(&temp.0).is_err());
    }

    #[test]
    fn orphan_direct_recovers_projection_and_keeps_offline_edits_and_tombstones() {
        let temp = Temp::new();
        let redie = server(&temp, "redie");
        let removed = server(&temp, "removed");
        enable_direct_at(&temp.0).unwrap();
        let mut catalog = EndpointCatalog::load_from_path(&temp.0.join("endpoints.json")).unwrap();
        catalog.rename_ssh(&redie.id, "Offline rename").unwrap();
        catalog.ssh.retain(|p| p.id != removed.id);
        catalog
            .store_to_path(&temp.0.join("endpoints.json"))
            .unwrap();
        let (epoch, document) = direct_snapshot_at(&temp.0).unwrap();
        assert!(document
            .records
            .iter()
            .any(|r| r.id == removed.id.to_string() && r.machine.is_none()));
        let mut state = load(&temp.0).unwrap().unwrap();
        state.pending_projection = Some(state.baseline.clone());
        save(&temp.0, &state).unwrap();
        catalog
            .rename_ssh(&redie.id, "Unrecovered old projection")
            .unwrap();
        catalog
            .store_to_path(&temp.0.join("endpoints.json"))
            .unwrap();
        assert!(enroll_with(&temp.0, |_| Ok(Some("a".repeat(32)))).unwrap());
        let state = load(&temp.0).unwrap().unwrap();
        assert_eq!(state.document, document);
        assert_eq!(state.baseline[0].label, "Offline rename");
        assert_eq!(
            state.source,
            Source::Ssh {
                target: redie.target
            }
        );
        assert_ne!(state.device, epoch);
        assert!(direct_merge_at(&temp.0, &epoch, &Document::default()).is_err());
    }

    #[test]
    fn direct_revision_changes_and_explicit_sources_fence_automatic_recovery() {
        let temp = Temp::new();
        server(&temp, "redie");
        enable_direct_at(&temp.0).unwrap();
        assert!(!enroll_with(&temp.0, |_| {
            let mut state = load(&temp.0).unwrap().unwrap();
            state.device = "f".repeat(32);
            save(&temp.0, &state).unwrap();
            Ok(Some("a".repeat(32)))
        })
        .unwrap());
        set_disabled(&temp.0, true).unwrap();
        assert!(!enroll_with(&temp.0, |_| panic!("explicit opt-out must win")).unwrap());
        set_disabled(&temp.0, false).unwrap();
        let mut state = load(&temp.0).unwrap().unwrap();
        state.source = Source::Local;
        save(&temp.0, &state).unwrap();
        assert!(!enroll_with(&temp.0, |_| panic!("explicit hosting must win")).unwrap());
    }

    #[test]
    fn old_connection_history_cannot_bypass_a_saved_profile_removed_during_discovery() {
        let temp = Temp::new();
        remember_at(&temp.0, "david@redie").unwrap();
        server(&temp, "redie");
        assert!(!enroll_with(&temp.0, |_| {
            let mut catalog =
                EndpointCatalog::load_from_path(&temp.0.join("endpoints.json")).unwrap();
            catalog.ssh.clear();
            catalog
                .store_to_path(&temp.0.join("endpoints.json"))
                .unwrap();
            Ok(Some("a".repeat(32)))
        })
        .unwrap());
        assert!(load(&temp.0).unwrap().is_none());
        assert!(remembered(&temp.0).unwrap().is_empty());
        assert!(!enroll_with(&temp.0, |_| panic!(
            "a removed saved profile must stay removed"
        ))
        .unwrap());
    }
}
