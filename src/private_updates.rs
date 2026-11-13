//! Opt-in signed private releases. No session protocol or catalog-key privileges.
mod feed;
mod install;
mod report;
#[cfg(test)]
mod tests;
mod transport;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const NAMESPACE: &str = "herdr-private-update-v1";
const MAX_ENVELOPE: u64 = 64 * 1024;
const MAX_BINARY: u64 = 512 * 1024 * 1024;
static RESTART: AtomicBool = AtomicBool::new(false);
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    channel: String,
    address: std::net::SocketAddr,
    signer: String,
    enabled: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    channel: String,
    release: u64,
    target: String,
    build_id: String,
    sha256: String,
    bytes: u64,
    protocol: u32,
    endpoint_generation: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    manifest: String,
    signature: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    channel: String,
    release: u64,
    sha256: String,
    build_id: String,
}
fn root() -> PathBuf {
    crate::config::state_dir().join("client/private-updates")
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn fail(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
fn save(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let data = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    crate::client::endpoint::store_private_json(path, &data, "private updates")
        .map_err(io::Error::other)
}
fn bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > limit {
        return Err(fail("Invalid or oversized update file"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(fail("Update file exceeds size limit"));
    }
    Ok(bytes)
}
fn json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    serde_json::from_slice(&bounded(path, MAX_ENVELOPE)?).map_err(io::Error::other)
}
fn policy_at(root: &Path) -> io::Result<Option<Policy>> {
    match json::<Policy>(&root.join("policy.json")) {
        Ok(p) => {
            p.validate()?;
            Ok(Some(p))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
impl Policy {
    fn validate(&self) -> io::Result<()> {
        if self.version != 1
            || self.channel.is_empty()
            || self.channel.len() > 64
            || !self
                .channel
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || !crate::client::tailscale::tailscale_ip(self.address.ip())
            || self.address.port() == 0
        {
            return Err(fail("Invalid private update channel"));
        }
        let fields: Vec<_> = self.signer.split_whitespace().collect();
        if fields.len() < 2
            || fields[0] != "ssh-ed25519"
            || self.signer.len() > 1024
            || self.signer.chars().any(char::is_control)
        {
            return Err(fail("Invalid pinned release signing key"));
        }
        Ok(())
    }
}
fn target() -> io::Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        _ => Err(fail(
            "Private automatic updates currently support Mac managers",
        )),
    }
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Manifest {
    fn validate(
        &self,
        policy: &Policy,
        expected_target: &str,
        accepted: Option<&Receipt>,
    ) -> io::Result<()> {
        if self.version != 1
            || self.channel != policy.channel
            || self.release == 0
            || self.target != expected_target
            || !valid_hash(&self.sha256)
            || self.bytes < 4096
            || self.bytes > MAX_BINARY
            || self.build_id.is_empty()
            || self.build_id.len() > 128
            || !self
                .build_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || self.protocol != crate::protocol::PROTOCOL_VERSION
            || self.endpoint_generation != crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION
        {
            return Err(fail(
                "Release is invalid or incompatible with this manager/runtime",
            ));
        }
        if let Some(old) = accepted {
            if (old.build_id == self.build_id && old.sha256 != self.sha256)
                || old.channel != self.channel
                || self.release < old.release
                || (self.release == old.release
                    && (self.sha256 != old.sha256 || self.build_id != old.build_id))
            {
                return Err(fail("Refusing replayed or conflicting private release"));
            }
        }
        Ok(())
    }
    fn receipt(&self) -> Receipt {
        Receipt {
            channel: self.channel.clone(),
            release: self.release,
            sha256: self.sha256.clone(),
            build_id: self.build_id.clone(),
        }
    }
}
fn digest(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
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
fn accepted_at(root: &Path) -> io::Result<Option<Receipt>> {
    match json(&root.join("accepted.json")) {
        Ok(r) => Ok(Some(r)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn verify(root: &Path, policy: &Policy, bytes: &[u8]) -> io::Result<Manifest> {
    use base64::Engine;
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    let manifest = base64::engine::general_purpose::STANDARD
        .decode(envelope.manifest)
        .map_err(io::Error::other)?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(envelope.signature)
        .map_err(io::Error::other)?;
    if manifest.len() > 16 * 1024 || signature.len() > 4096 {
        return Err(fail("Oversized release signature"));
    }
    let keys = root.join("allowed-signers");
    let sig = root.join("manifest.sig");
    crate::client::endpoint::store_private_json(
        &keys,
        format!("herdr-release {}\n", policy.signer).as_bytes(),
        "release signer",
    )
    .map_err(io::Error::other)?;
    crate::client::endpoint::store_private_json(&sig, &signature, "release signature")
        .map_err(io::Error::other)?;
    let mut child = std::process::Command::new("ssh-keygen")
        .args(["-Y", "verify", "-f"])
        .arg(&keys)
        .args(["-I", "herdr-release", "-n", NAMESPACE, "-s"])
        .arg(&sig)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| fail("No signature verifier input"))?
        .write_all(&manifest)?;
    if !child.wait()?.success() {
        return Err(fail(
            "Private update signature did not match the pinned signer",
        ));
    }
    serde_json::from_slice(&manifest).map_err(io::Error::other)
}
pub(crate) fn enabled() -> bool {
    if cfg!(test) {
        return false;
    }
    policy_at(&root()).ok().flatten().is_some_and(|p| p.enabled)
}
pub(crate) fn managed_executable() -> Option<PathBuf> {
    if !enabled() {
        return None;
    }
    let path = destination().ok()?;
    let installed: Receipt = json(&root().join("installed.json")).ok()?;
    let accepted = accepted_at(&root()).ok().flatten()?;
    if bounded_metadata(&path).is_ok()
        && installed.channel == policy_at(&root()).ok().flatten()?.channel
        && installed.channel == accepted.channel
        && installed.release == accepted.release
        && installed.sha256 == accepted.sha256
        && digest(&path).ok().as_deref() == Some(&installed.sha256)
    {
        Some(path)
    } else {
        None
    }
}
pub(crate) fn background_restart(previous: &mut Option<Vec<u8>>) -> io::Result<()> {
    let receipt = bounded(&root().join("installed.json"), MAX_ENVELOPE).ok();
    if *previous == receipt {
        return Ok(());
    }
    *previous = receipt;
    let _lease = install_lock()?;
    let path = destination()?;
    if ready_at(&root(), &path, crate::build_info::build_id())? {
        crate::platform::private_updates::reexec(&path)?;
    }
    Ok(())
}
pub(crate) fn install_lock() -> io::Result<fs::File> {
    let file = crate::client::direct_sync::worker_lease(
        &crate::config::state_dir().join("client"),
        "binary-install.lock",
    )?;
    file.lock()?;
    Ok(file)
}
fn check_at(
    root: &Path,
    destination: &Path,
    policy: &Policy,
    expected_target: &str,
) -> io::Result<bool> {
    check_with(
        root,
        destination,
        policy,
        expected_target,
        || transport::get(policy.address, "/v1/current", MAX_ENVELOPE),
        |manifest, file| {
            transport::download(
                policy.address,
                &format!("/v1/binary/{}", manifest.sha256),
                manifest.bytes,
                file,
            )
        },
    )
}
fn check_with(
    root: &Path,
    destination: &Path,
    policy: &Policy,
    expected_target: &str,
    fetch: impl FnOnce() -> io::Result<Vec<u8>>,
    download: impl FnOnce(&Manifest, &mut fs::File) -> io::Result<()>,
) -> io::Result<bool> {
    policy.validate()?;
    let lease = crate::client::direct_sync::worker_lease(root, "check.lock")?;
    if lease.try_lock().is_err() {
        return Ok(false);
    }
    let bytes = fetch()?;
    let manifest = verify(root, policy, &bytes)?;
    manifest.validate(policy, expected_target, accepted_at(root)?.as_ref())?;
    if accepted_at(root)?.is_some_and(|r| r.release == manifest.release)
        && destination.is_file()
        && digest(destination)? == manifest.sha256
    {
        let _install = install_lock()?;
        if policy_at(root)?.as_ref() != Some(policy) || !policy.enabled {
            return Err(fail("Update settings changed during download"));
        }
        manifest.validate(policy, expected_target, accepted_at(root)?.as_ref())?;
        save(&root.join("installed.json"), &manifest.receipt())?;
        return Ok(true);
    }
    let temp = root.join("download.stage");
    if temp.exists() {
        fs::remove_file(&temp)?;
    }
    let mut file = crate::platform::create_private_state_file(&temp)?;
    let result = download(&manifest, &mut file);
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    file.sync_all()?;
    drop(file);
    if digest(&temp)? != manifest.sha256 {
        let _ = fs::remove_file(&temp);
        return Err(fail("Private update checksum mismatch"));
    }
    let _install = install_lock()?;
    if policy_at(root)?.as_ref() != Some(policy) || !policy.enabled {
        return Err(fail("Update settings changed during download"));
    }
    manifest.validate(policy, expected_target, accepted_at(root)?.as_ref())?;
    // High-water mark is durable before replacement. Equal-counter retry recovers interruptions.
    save(&root.join("accepted.json"), &manifest.receipt())?;
    install::promote(&temp, destination)?;
    save(&root.join("installed.json"), &manifest.receipt())?;
    let _ = fs::remove_file(temp);
    Ok(true)
}
fn destination() -> io::Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("HOME").ok_or_else(|| fail("HOME unavailable"))?)
            .join(".local/bin/herdr"),
    )
}
pub(crate) fn check() -> io::Result<()> {
    run_check(false)
}
fn run_check(automatic: bool) -> io::Result<()> {
    let root = root();
    let lease = crate::client::direct_sync::worker_lease(&root, "run.lock")?;
    if lease.try_lock().is_err() {
        return Ok(());
    }
    let Some(policy) = policy_at(&root)?.filter(|p| p.enabled) else {
        return Ok(());
    };
    let result = check_at(&root, &destination()?, &policy, target()?);
    if matches!(result, Ok(false)) {
        return Ok(());
    }
    save(
        &root.join("status.json"),
        &serde_json::json!({"checked_at":now(),"pid":std::process::id(),"automatic":automatic,"error":result.as_ref().err().map(ToString::to_string)}),
    )?;
    let _ = report::send(&root, &policy);
    result.map(|_| ())
}
fn automatic_due(value: &serde_json::Value, at: u64) -> bool {
    value["automatic"] != true
        || value["checked_at"]
            .as_u64()
            .is_none_or(|last| at < last || at - last >= 60)
}
pub(crate) fn start_worker(quit: Arc<AtomicBool>, service: bool) {
    if cfg!(test) {
        return;
    }
    std::thread::spawn(move || {
        let mut owner = None;
        while !quit.load(Ordering::Acquire) {
            let status = service_status();
            let resident =
                !service && status["running"] == true && status["private_updates"] == true;
            if resident {
                owner = None;
            } else if owner.is_none() {
                if let Ok(lease) = crate::client::direct_sync::worker_lease(&root(), "worker.lock")
                {
                    if lease.try_lock().is_ok() {
                        owner = Some(lease);
                    }
                }
            }
            if owner.is_some() {
                let previous: serde_json::Value =
                    json(&root().join("status.json")).unwrap_or_default();
                let requested: serde_json::Value =
                    json(&root().join("check-request.json")).unwrap_or_default();
                let forced = requested["at"]
                    .as_u64()
                    .is_some_and(|at| previous["checked_at"].as_u64().is_none_or(|last| last < at));
                if forced
                    || (service && previous["pid"].as_u64() != Some(u64::from(std::process::id())))
                    || automatic_due(&previous, now())
                {
                    if let Err(error) = run_check(true) {
                        tracing::warn!(%error,"private update paused");
                    }
                }
            }
            for _ in 0..5 {
                if quit.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });
}
fn install_service(exe: &Path, hash: &str) -> io::Result<()> {
    fs::create_dir_all(crate::config::config_dir())?;
    let lease = crate::client::direct_sync::worker_lease(&root(), "registration.lock")?;
    lease.lock()?;
    crate::platform::private_updates::install_service(exe, hash)
}
pub(crate) fn ensure_started() {
    if cfg!(test)
        || cfg!(debug_assertions)
        || !enabled()
        || std::env::var_os("HERDR_UPDATE_SERVICE").is_some()
    {
        return;
    }
    std::thread::spawn(|| {
        if let Some(exe) = managed_executable() {
            if let Ok(hash) = digest(&exe) {
                if let Err(error) = install_service(&exe, &hash) {
                    tracing::warn!(%error,"Private update service registration failed; UI fallback remains available");
                }
            }
        }
    });
}
fn service_status() -> serde_json::Value {
    let mut value: serde_json::Value = json(&root().join("service.json")).unwrap_or_default();
    let fresh = value["heartbeat"]
        .as_u64()
        .is_some_and(|at| now() >= at && now() - at <= 20);
    let alive = value["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .is_some_and(crate::platform::process_exists);
    if !value.is_object() {
        return serde_json::json!({"running":false});
    }
    value["running"] = (fresh && alive).into();
    value
}
fn serve_worker() -> io::Result<()> {
    let lease = crate::client::direct_sync::worker_lease(&root(), "service.lock")?;
    if lease.try_lock().is_err() {
        return Ok(());
    }
    let binary = digest(&std::env::current_exe()?)?;
    let quit = Arc::new(AtomicBool::new(false));
    start_worker(quit, true);
    let mut previous = None;
    loop {
        save(
            &root().join("service.json"),
            &serde_json::json!({"pid":std::process::id(),"heartbeat":now(),"binary":binary,"private_updates":true}),
        )?;
        background_restart(&mut previous)?;
        std::thread::sleep(Duration::from_secs(5));
    }
}
fn ready_service(
    service: &serde_json::Value,
    updater: &serde_json::Value,
    expected: &str,
    since: u64,
) -> bool {
    service["running"] == true
        && service["pid"].as_u64().is_some()
        && service["binary"].as_str() == Some(expected)
        && service["private_updates"] == true
        && service["heartbeat"].as_u64().is_some_and(|at| at >= since)
        && updater["automatic"] == true
        && updater["pid"] == service["pid"]
        && updater["error"].is_null()
        && updater["checked_at"].as_u64().is_some_and(|at| at >= since)
}
fn await_service(expected: &str, since: u64) -> io::Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    loop {
        let service = service_status();
        let updater: serde_json::Value = json(&root().join("status.json")).unwrap_or_default();
        if ready_service(&service, &updater, expected, since) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(fail(format!("Background registration completed, but the expected updater did not confirm a successful check. Service: {service}; last updater result: {updater}. The UI can keep checking while open.")));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
pub(crate) fn status_line() -> String {
    let root = root();
    match policy_at(&root) {
        Ok(Some(p)) if p.enabled => {
            let status: serde_json::Value = json(&root.join("status.json")).unwrap_or_default();
            if let Some(error) = status["error"].as_str() {
                format!("Updates via Redie: {error}")
            } else if let Ok(r) = json::<Receipt>(&root.join("installed.json")) {
                format!("Updates via Redie: automatic · {}", r.build_id)
            } else {
                "Updates via Redie: checking…".into()
            }
        }
        _ => "Automatic private updates are off".into(),
    }
}
fn ready_at(root: &Path, destination: &Path, running_build: Option<&str>) -> io::Result<bool> {
    let Some(p) = policy_at(root)?.filter(|p| p.enabled) else {
        return Ok(false);
    };
    let installed: Receipt = match json(&root.join("installed.json")) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if running_build == Some(installed.build_id.as_str()) {
        return Ok(false);
    }
    let Some(accepted) = accepted_at(root)? else {
        return Ok(false);
    };
    Ok(installed.channel == p.channel
        && installed.release == accepted.release
        && installed.sha256 == accepted.sha256
        && installed.build_id == accepted.build_id
        && valid_hash(&installed.sha256)
        && bounded_metadata(destination)?.is_file()
        && digest(destination)? == installed.sha256)
}
fn bounded_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > MAX_BINARY {
        return Err(fail("Invalid installed update path"));
    }
    Ok(m)
}
pub(crate) fn watch(notify: impl Fn() + Send + 'static, quit: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut checked = None;
        while !quit.load(Ordering::Acquire) {
            // Hash only when the receipt changes, never on render or every polling tick.
            let receipt = bounded(&root().join("installed.json"), MAX_ENVELOPE).ok();
            if receipt != checked {
                checked = receipt;
                if destination()
                    .and_then(|p| ready_at(&root(), &p, crate::build_info::build_id()))
                    .unwrap_or(false)
                {
                    notify();
                    return;
                }
            }
            for _ in 0..5 {
                if quit.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });
}
pub(crate) fn request_restart() {
    RESTART.store(true, Ordering::Release);
}
pub(crate) fn restart_if_requested() -> io::Result<()> {
    if !RESTART.swap(false, Ordering::AcqRel) {
        return Ok(());
    }
    let _lease = install_lock()?;
    let path = destination()?;
    if ready_at(&root(), &path, crate::build_info::build_id())? {
        crate::platform::private_updates::reexec(&path)?;
    }
    Ok(())
}
pub(crate) fn run(args: &[String]) -> io::Result<i32> {
    match args {
        [a] if a == "enable" => {
            let _ = target()?;
            let policy: Policy =
                serde_json::from_str(include_str!("../assets/private-update-channel.json"))
                    .map_err(io::Error::other)?;
            policy.validate()?;
            let root = root();
            if let Some(mut old) = policy_at(&root)? {
                old.enabled = true;
                if old != policy {
                    return Err(fail("A different private update trust policy is already pinned; preserve it or explicitly reconfigure locally"));
                }
            }
            save(&root.join("policy.json"), &policy)?;
            check()?;
            let since = now();
            save(
                &root.join("check-request.json"),
                &serde_json::json!({"at":since}),
            )?;
            let expected = digest(&destination()?)?;
            install_service(&destination()?, &expected)?;
            await_service(&expected, since)?;
            println!("Automatic signed updates from Redie enabled. Run ~/.local/bin/herdr.");
        }
        [a] if a == "disable" => {
            let _lease = install_lock()?;
            if let Some(mut p) = policy_at(&root())? {
                p.enabled = false;
                save(&root().join("policy.json"), &p)?;
            }
            println!("Automatic private updates disabled; server-list sync remains enabled.");
        }
        [a] if a == "status" => println!("{}", status_line()),
        [a] if a == "check" => {
            check()?;
            println!("{}", status_line());
        }
        [a] if a == "worker" => serve_worker()?,
        [a] if a == "peers" => println!("{}", report::peers()?),
        [a, bind] if a == "serve" => feed::serve(bind.parse().map_err(io::Error::other)?)?,
        _ => {
            return Err(fail(
                "usage: herdr updates <enable|disable|status|check|serve TAILSCALE-IP:PORT>",
            ))
        }
    }
    Ok(0)
}
