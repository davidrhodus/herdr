//! Discover an account that already opted into hosting. Never provision a host
//! or override explicit local sync/pairing decisions from a background probe.
use super::*;

const DISABLED: &str = "catalog-sync-disabled.json";

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
    let profiles = {
        let _guard = lock(root)?;
        if load(root)?.is_some() || disabled(root) {
            return Ok(false);
        }
        EndpointCatalog::load_from_path(&root.join("endpoints.json"))?.ssh
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
        if load(root)?.is_some() || disabled(root) {
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
        let current = EndpointCatalog::load_from_path(&root.join("endpoints.json"))?;
        if !current
            .ssh
            .iter()
            .any(|p| p.id == profile.id && p.target == profile.target && p.enabled)
        {
            return Ok(false);
        }
        let mut state = SyncState {
            version: 1,
            source: Source::Ssh {
                target: profile.target.clone(),
            },
            device: ProfileId::generate().to_string(),
            document: Document::default(),
            baseline: Vec::new(),
            pending_projection: None,
            last_success: None,
            last_error: None,
        };
        state.stage(&current.ssh)?;
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
}
