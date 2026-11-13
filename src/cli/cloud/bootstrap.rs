use super::{model::*, transport::*};
use serde_json::json;
use std::{
    io,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

pub(super) fn enrollment(settings: &Settings) -> io::Result<String> {
    token(&settings.oauth_client)?;
    if settings.tailnet != "-" {
        token(&settings.tailnet)?;
    }
    token(&settings.tag)?;
    if !settings.tag.starts_with("tag:") {
        return Err(error("Configure a Tailscale server tag first"));
    }
    let path = root()?.join("tailscale-oauth-secret");
    crate::platform::validate_private_key_path(&path, false)?;
    let secret = std::fs::read_to_string(path)?;
    let form = format!(
        "client_id={}&client_secret={}",
        encode(&settings.oauth_client),
        encode(secret.trim())
    );
    let access = http(
        "POST",
        "https://api.tailscale.com/api/v2/oauth/token",
        None,
        None,
        Some(&form),
    )?;
    let body = json!({"capabilities":{"devices":{"create":{"reusable":false,"ephemeral":false,"preauthorized":true,"tags":[settings.tag]}}},"expirySeconds":86400,"description":"Herdr instance enrollment"});
    let reply = http(
        "POST",
        &format!(
            "https://api.tailscale.com/api/v2/tailnet/{}/keys",
            encode(&settings.tailnet)
        ),
        Some(field(&access, "access_token")?),
        Some(&body),
        None,
    )?;
    Ok(field(&reply, "key")?.into())
}
pub(super) fn identity(id: &str) -> io::Result<String> {
    let path = job_dir(id)?.join("id_ed25519");
    if !path.exists() {
        let mut c = Command::new("ssh-keygen");
        c.args(["-q", "-t", "ed25519", "-N", "", "-C", "herdr-cloud", "-f"])
            .arg(&path);
        capture(c, &[], Duration::from_secs(30))?;
    }
    crate::platform::validate_private_key_path(&path, false)?;
    let public = std::fs::read_to_string(path.with_extension("pub"))?;
    validate_public(public.trim())?;
    Ok(public.trim().into())
}
fn validate_public(public: &str) -> io::Result<()> {
    use base64::Engine;
    let parts: Vec<_> = public.split_whitespace().collect();
    if parts.len() < 2
        || parts[0] != "ssh-ed25519"
        || public.len() > 512
        || base64::engine::general_purpose::STANDARD
            .decode(parts[1])
            .is_err()
    {
        return Err(error("Invalid Ed25519 host key"));
    }
    Ok(())
}
/// Idempotent first boot. No xtrace: the one-use auth key must not reach serial logs.
pub(super) fn script(id: &str, public: &str, auth: &str, hostname: &str) -> io::Result<String> {
    token(id)?;
    token(hostname)?;
    validate_public(public)?;
    if !auth.starts_with("tskey-auth-")
        || !auth.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(error("Invalid Tailscale enrollment key"));
    }
    let script = include_str!("bootstrap.sh")
        .replace("@JOB@", id)
        .replace("@PUBLIC@", public)
        .replace("@AUTH@", auth)
        .replace("@HOSTNAME@", hostname);
    Ok(script)
}
#[derive(Debug, PartialEq)]
pub(super) struct Proof {
    pub node: String,
    pub ip: std::net::Ipv4Addr,
    pub public: String,
}
pub(super) fn proof(console: &str, id: &str) -> io::Result<Option<Proof>> {
    for line in console.lines().rev() {
        let Some(rest) = line.strip_prefix(&format!("HERDR-READY {id} ")) else {
            continue;
        };
        let p: Vec<_> = rest.split_whitespace().collect();
        if p.len() < 4 {
            continue;
        }
        let public = format!("{} {}", p[2], p[3]);
        validate_public(&public)?;
        let ip: std::net::Ipv4Addr = p[1]
            .parse()
            .map_err(|_| error("Invalid Tailscale address from instance"))?;
        if !(ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])) {
            return Err(error("Instance reported an address outside Tailscale"));
        }
        token(p[0])?;
        return Ok(Some(Proof {
            node: p[0].into(),
            ip,
            public,
        }));
    }
    Ok(None)
}
fn pin(path: &Path, ip: &str, public: &str) -> io::Result<()> {
    // Never replace a prior key for this job: a mismatch requires explicit inspection.
    let expected = format!("{ip} {public}\n");
    if path.exists() {
        crate::platform::validate_private_key_path(path, false)?;
        if std::fs::read_to_string(path)? != expected {
            return Err(error(
                "Instance host key changed. Refusing to overwrite the pinned key.",
            ));
        }
        return Ok(());
    }
    write(path, expected.as_bytes())
}
pub(super) fn wait_proof(job: &Job) -> io::Result<Proof> {
    wait_proof_for(job, &job.id)
}
pub(super) fn wait_proof_for(job: &Job, identity: &str) -> io::Result<Proof> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(900) {
        if let Ok(console) = super::provider::console(job) {
            if let Some(proof) = proof(&console, identity)? {
                let status = json(&crate::client::tailscale::status_bytes().map_err(error)?)?;
                if status["Peer"].as_object().is_some_and(|peers| {
                    peers.values().any(|p| {
                        p["Online"] == true
                            && p["ID"] == proof.node
                            && p["TailscaleIPs"].as_array().is_some_and(|ips| {
                                ips.iter()
                                    .any(|ip| ip.as_str() == Some(&proof.ip.to_string()))
                            })
                    })
                }) {
                    return Ok(proof);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    Err(error("Bootstrap is not ready. Check outbound internet, Tailscale tag approval and SSH policy. Resume this retained job when ready; it will not create another VM."))
}
pub(super) fn connect(job: &mut Job, proof: Proof) -> io::Result<()> {
    let target = trust(job, &proof)?;
    job.target = Some(target.clone());
    job.phase("Installing and checking the Rust terminal server")?;
    // Reuse the current build's bundled Linux payload and existing approval/health checks.
    let setup = crate::remote::SavedSshSetup::connect(&target)?;
    let metadata = setup.prepare(crate::session::DEFAULT_SESSION_NAME)?;
    crate::remote::check_saved_ssh(&target, crate::session::DEFAULT_SESSION_NAME)?;
    let mut catalog = super::super::machine::load_catalog()?;
    let id = if let Some(p) = catalog
        .ssh
        .iter()
        .find(|p| p.target == target && p.session == crate::session::DEFAULT_SESSION_NAME)
    {
        p.id.clone()
    } else {
        catalog
            .add_ssh(
                job.name.clone(),
                &target,
                crate::session::DEFAULT_SESSION_NAME,
            )
            .map_err(error)?
    };
    super::super::machine::store_catalog(&catalog)?;
    if let Some(metadata) = metadata {
        crate::client::endpoint::SshMetadataCache::new(
            id.as_str(),
            &target,
            crate::session::DEFAULT_SESSION_NAME,
        )?
        .store(&metadata);
    }
    job.endpoint = Some(id.to_string());
    job.phase("Ready · saved in Servers and shared by normal server-list sync")
}
pub(super) fn trust(job: &Job, proof: &Proof) -> io::Result<String> {
    let dir = job_dir(&job.id)?;
    let ip = proof.ip.to_string();
    let hosts = dir.join(format!("known_hosts-{ip}"));
    pin(&hosts, &ip, &proof.public)?;
    let target = format!("herdr@{ip}");
    crate::remote::device_identity::set(&target, &dir.join("id_ed25519").to_string_lossy())
        .map_err(error)?;
    crate::remote::device_identity::set_host_keys(&target, &hosts).map_err(error)?;
    Ok(target)
}
#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBmzjzIg56nCCmzHFIVC5o1QYrxPRVKD5htkzEVf54yH";
    #[test]
    fn console_proof_requires_job_identity_and_tailnet_address() {
        assert!(
            proof(&format!("HERDR-READY other node 100.64.1.2 {KEY}"), "mine")
                .unwrap()
                .is_none()
        );
        assert!(proof(&format!("HERDR-READY mine node 1.2.3.4 {KEY}"), "mine").is_err());
        assert_eq!(
            proof(&format!("HERDR-READY mine node 100.64.1.2 {KEY}"), "mine")
                .unwrap()
                .unwrap()
                .node,
            "node"
        );
    }
    #[test]
    fn bootstrap_has_no_long_lived_credentials_or_shell_interpolation() {
        let s = script("job", KEY, "tskey-auth-test", "herdr-test").unwrap();
        assert!(s.contains("--auth-key=file:"));
        assert!(!s.contains("client_secret"));
        assert!(!s.contains("set -x"));
        assert!(script("job", KEY, "tskey-auth-$(evil)", "host").is_err());
    }
}
