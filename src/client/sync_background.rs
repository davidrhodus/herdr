//! User-owned sync service: no terminals, workspaces, or rendering are started.
use super::endpoint::catalog_sync;
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) fn root() -> PathBuf {
    crate::config::state_dir().join("client")
}
fn installed_executable() -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let stable = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/bin/herdr"));
    if stable.as_ref().is_some_and(|path| {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
            && std::fs::canonicalize(path).ok() == std::fs::canonicalize(&executable).ok()
    }) {
        return Ok(stable.expect("checked stable path"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is unavailable"))?;
    let manifest = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let canonical_home = std::fs::canonicalize(&home)?;
    let executable = std::fs::canonicalize(executable)?;
    let ordinary_namespace = crate::config::config_dir() == home.join(".config/herdr")
        && crate::config::state_dir() == home.join(".local/state/herdr");
    if !cfg!(test)
        && !cfg!(debug_assertions)
        && cargo_release_layout(&executable, &manifest, &canonical_home, ordinary_namespace)
    {
        let stable = home.join(".local/libexec/herdr-sync-agent");
        install_cargo_binary(&executable, &stable)?;
        return Ok(stable);
    }
    Err(io::Error::other("Install this Herdr build at ~/.local/bin/herdr, or run cargo run --release from ~/herdr, before enabling background sync"))
}
fn cargo_release_layout(
    executable: &Path,
    manifest: &Path,
    home: &Path,
    ordinary_namespace: bool,
) -> bool {
    ordinary_namespace
        && manifest.starts_with(home)
        && executable == manifest.join("target/release/herdr")
}
fn install_cargo_binary(executable: &Path, stable: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(stable).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
        && executable_fingerprint(executable)? == executable_fingerprint(stable)?
    {
        return Ok(());
    }
    std::fs::create_dir_all(
        stable
            .parent()
            .ok_or_else(|| io::Error::other("Invalid installed path"))?,
    )?;
    if stable.is_file() {
        let backup = root().join("sync-agent-backup");
        std::fs::create_dir_all(&backup)?;
        std::fs::copy(stable, backup.join("herdr.previous"))?;
    }
    let temp = stable.with_extension(format!("sync-install-{}", std::process::id()));
    let mut file = crate::platform::create_private_state_file(&temp)?;
    let mut source = std::fs::File::open(executable)?;
    io::copy(&mut source, &mut file)?;
    file.sync_all()?;
    std::fs::set_permissions(&temp, std::fs::metadata(executable)?.permissions())?;
    drop(file);
    crate::platform::replace_file(&temp, stable)
}

pub(crate) fn start() -> io::Result<()> {
    start_with_policy(true)
}
fn start_with_policy(manual: bool) -> io::Result<()> {
    let lease = super::direct_sync::worker_lease(&root(), "sync-install.lock")?;
    lease.lock()?;
    let disabled = root().join("background-sync-disabled.json");
    if !manual && disabled.exists() {
        return Ok(());
    }
    if manual {
        match std::fs::remove_file(&disabled) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let executable = installed_executable()?;
    std::fs::create_dir_all(crate::config::config_dir())?;
    let identity = format!(
        "{}:{}",
        crate::build_info::version(),
        executable_fingerprint(&executable)?
    );
    crate::platform::sync_service::install(
        &executable,
        &crate::config::config_dir(),
        &crate::config::state_dir(),
        &identity,
    )
}

pub(crate) fn ensure_started() {
    // Development/test clients must never register login services.
    if cfg!(test) || cfg!(debug_assertions) || std::env::var_os("HERDR_SYNC_SERVICE").is_some() {
        return;
    }
    std::thread::spawn(|| {
        if let Err(error) = start_with_policy(false) {
            tracing::warn!(%error,"Background sync unavailable; UI sync remains active");
        }
    });
}
pub(crate) fn stop() -> io::Result<()> {
    let lease = super::direct_sync::worker_lease(&root(), "sync-install.lock")?;
    lease.lock()?;
    super::endpoint::store_private_json(
        &root().join("background-sync-disabled.json"),
        b"true\n",
        "background sync opt-out",
    )
    .map_err(io::Error::other)?;
    crate::platform::sync_service::stop()
}
pub(crate) fn status() -> serde_json::Value {
    status_at(&root())
}
fn status_at(root: &Path) -> serde_json::Value {
    let value = std::fs::File::open(root.join("sync-service-status.json"))
        .ok()
        .and_then(|file| {
            let mut bytes = Vec::new();
            file.take(8192).read_to_end(&mut bytes).ok()?;
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .filter(serde_json::Value::is_object)
        });
    let Some(mut value) = value else {
        return serde_json::json!({"running":false});
    };
    let last = value["heartbeat"].as_u64().unwrap_or(0);
    let current = now();
    value["running"] = serde_json::Value::Bool(
        last > 0
            && current >= last
            && current - last <= 20
            && value["pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
                .is_some_and(crate::platform::process_exists),
    );
    if let Some(network) = std::fs::read(root.join("catalog-network-status.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    {
        if network["pid"] == value["pid"] {
            value["network"] = network;
        }
    }
    value
}
fn executable_fingerprint(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn serve() -> io::Result<()> {
    crate::logging::init_file_logging("herdr-sync.log");
    let root = root();
    let lease = super::direct_sync::worker_lease(&root, "sync-service.lock")?;
    if lease.try_lock().is_err() {
        return Ok(());
    }
    let quit = Arc::new(AtomicBool::new(false));
    let binary = executable_fingerprint(&std::env::current_exe()?)?;
    catalog_sync::start_worker(quit.clone());
    super::direct_sync::start_background(quit);
    loop {
        let value = serde_json::json!({"pid":std::process::id(),"heartbeat":now(),"version":crate::build_info::version(),"binary":binary});
        super::endpoint::store_private_json(
            &root.join("sync-service-status.json"),
            &serde_json::to_vec(&value).map_err(io::Error::other)?,
            "sync service",
        )
        .map_err(io::Error::other)?;
        std::thread::sleep(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cargo_release_bootstrap_excludes_debug_binaries_and_isolated_test_namespaces() {
        let home = Path::new("/Users/a");
        let manifest = home.join("herdr");
        let executable = manifest.join("target/release/herdr");
        assert!(cargo_release_layout(&executable, &manifest, home, true));
        assert!(!cargo_release_layout(&executable, &manifest, home, false));
        assert!(!cargo_release_layout(
            &manifest.join("target/debug/herdr"),
            &manifest,
            home,
            true
        ));
        assert!(!cargo_release_layout(
            &executable,
            &manifest,
            Path::new("/test/home"),
            true
        ));
    }
    #[test]
    fn same_version_replacement_changes_service_build_identity() {
        let path = std::env::temp_dir().join(format!(
            "herdr-fingerprint-{}",
            super::super::endpoint::ProfileId::generate()
        ));
        std::fs::write(&path, b"first binary").unwrap();
        let first = format!("0.9.5:{}", executable_fingerprint(&path).unwrap());
        std::fs::write(&path, b"replacement binary").unwrap();
        let second = format!("0.9.5:{}", executable_fingerprint(&path).unwrap());
        assert_ne!(first, second);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn stale_background_status_never_claims_a_live_service() {
        let root = std::env::temp_dir().join(format!(
            "herdr-service-status-{}",
            super::super::endpoint::ProfileId::generate()
        ));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(status_at(&root)["running"], false);
        std::fs::write(
            root.join("sync-service-status.json"),
            br#"{"heartbeat":1,"pid":1}"#,
        )
        .unwrap();
        assert_eq!(status_at(&root)["running"], false);
        std::fs::write(
            root.join("sync-service-status.json"),
            serde_json::to_vec(&serde_json::json!({"heartbeat":now(),"pid":std::process::id()}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(status_at(&root)["running"], true);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn multiple_workers_elect_one_network_owner_per_state_namespace() {
        let root = std::env::temp_dir().join(format!(
            "herdr-service-owner-{}",
            super::super::endpoint::ProfileId::generate()
        ));
        let first =
            super::super::direct_sync::worker_lease(&root, "catalog-sync-owner.lock").unwrap();
        let second =
            super::super::direct_sync::worker_lease(&root, "catalog-sync-owner.lock").unwrap();
        first.try_lock().unwrap();
        assert!(second.try_lock().is_err());
        drop(first);
        second.try_lock().unwrap();
        drop(second);
        std::fs::remove_dir_all(root).unwrap();
    }
}
