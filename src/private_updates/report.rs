//! Bounded, client-reported update diagnostics. Reports grant no update authority.
use super::*;
use std::net::{IpAddr, SocketAddr, TcpStream};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    version: u32,
    enabled: bool,
    automatic: bool,
    running_build: Option<String>,
    installed: Option<Receipt>,
    error: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportV2 {
    version: u32,
    status: Report,
    display_version: String,
}
#[derive(Serialize, Deserialize)]
struct Seen {
    address: IpAddr,
    received_at: u64,
    report: Report,
    #[serde(default)]
    display_version: Option<String>,
}
fn valid(report: &Report) -> bool {
    let text = |s: &str, limit| s.len() <= limit && !s.chars().any(char::is_control);
    report.version == 1
        && report.running_build.as_ref().is_none_or(|v| text(v, 128))
        && report.error.as_ref().is_none_or(|e| text(e, 512))
        && report.installed.as_ref().is_none_or(|r| {
            r.release > 0 && valid_hash(&r.sha256) && text(&r.build_id, 128) && text(&r.channel, 64)
        })
}
pub(super) fn send(root: &Path, policy: &Policy) -> io::Result<()> {
    let status: serde_json::Value = json(&root.join("status.json"))?;
    let report = Report {
        version: 1,
        enabled: policy_at(root)?.is_some_and(|p| p.enabled),
        automatic: status["automatic"] == true,
        running_build: crate::build_info::build_id().map(str::to_owned),
        installed: json(&root.join("installed.json")).ok(),
        error: status["error"]
            .as_str()
            .map(|s| s.chars().filter(|c| !c.is_control()).take(512).collect()),
    };
    let bytes = serde_json::to_vec(&ReportV2 {
        version: 2,
        status: report.clone(),
        display_version: crate::build_info::DISPLAY_VERSION.into(),
    })
    .map_err(io::Error::other)?;
    let response = post(policy, "/v2/report", &bytes)?;
    if response.starts_with(b"HTTP/1.1 404 Not Found\r\n") {
        let bytes = serde_json::to_vec(&report).map_err(io::Error::other)?;
        let response = post(policy, "/v1/report", &bytes)?;
        if response.starts_with(b"HTTP/1.1 200 OK\r\n") {
            return Ok(());
        }
    } else if response.starts_with(b"HTTP/1.1 200 OK\r\n") {
        return Ok(());
    }
    Err(fail("Update host did not accept manager status"))
}
fn post(policy: &Policy, path: &str, bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&policy.address, Duration::from_secs(5))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        policy.address,
        bytes.len()
    )?;
    stream.write_all(bytes)?;
    transport::headers(&mut stream)
}
fn decode(bytes: &[u8], v2: bool) -> io::Result<(Report, Option<String>)> {
    let (report, display) = if v2 {
        let envelope: ReportV2 = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        if envelope.version != 2
            || envelope.display_version.is_empty()
            || envelope.display_version.len() > 64
            || envelope.display_version.chars().any(char::is_control)
        {
            return Err(fail("Invalid manager version"));
        }
        (envelope.status, Some(envelope.display_version))
    } else {
        (
            serde_json::from_slice(bytes).map_err(io::Error::other)?,
            None,
        )
    };
    if !valid(&report) {
        return Err(fail("Invalid update report"));
    }
    Ok((report, display))
}

fn read_body(stream: &mut TcpStream, length: usize, timeout: Duration) -> io::Result<Vec<u8>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut bytes = vec![0; length];
    let mut offset = 0;
    while offset < length {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| fail("Update report deadline exceeded"))?;
        stream.set_read_timeout(Some(remaining))?;
        let n = stream.read(&mut bytes[offset..])?;
        if n == 0 {
            return Err(fail("Truncated update report"));
        }
        offset += n;
    }
    Ok(bytes)
}
pub(super) fn receive(
    root: &Path,
    peer: SocketAddr,
    header: &str,
    stream: &mut TcpStream,
    v2: bool,
) -> io::Result<()> {
    if !crate::client::tailscale::tailscale_ip(peer.ip()) {
        return Err(fail("Update report must come from Tailscale"));
    }
    let mut length = None;
    for line in header.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| fail("Invalid report header"))?;
        if key.eq_ignore_ascii_case("transfer-encoding") {
            return Err(fail("Chunked reports unsupported"));
        }
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(fail("Duplicate report length"));
            }
            length = Some(value.trim().parse::<usize>().map_err(io::Error::other)?);
        }
    }
    let n = length
        .filter(|n| *n > 0 && *n <= 4096)
        .ok_or_else(|| fail("Invalid report size"))?;
    let bytes = read_body(stream, n, Duration::from_secs(3))?;
    let (report, display_version) = decode(&bytes, v2)?;
    let lease = crate::client::direct_sync::worker_lease(root, "reports.lock")?;
    if lease.try_lock().is_err() {
        return Err(fail("Update diagnostics busy"));
    }
    let path = root.join("manager-reports.json");
    let mut reports: Vec<Seen> = json(&path).unwrap_or_default();
    reports.retain(|r| r.address != peer.ip());
    reports.sort_by_key(|r| r.received_at);
    if reports.len() >= 32 {
        reports.drain(..reports.len() - 31);
    }
    reports.push(Seen {
        address: peer.ip(),
        received_at: now(),
        report,
        display_version,
    });
    save(&path, &reports)?;
    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}
pub(super) fn peers() -> io::Result<serde_json::Value> {
    peers_at(&crate::config::state_dir().join("private-update-feed"))
}
pub(super) fn peers_at(root: &Path) -> io::Result<serde_json::Value> {
    use base64::Engine;
    let rows: Vec<Seen> = match json(&root.join("manager-reports.json")) {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(e),
    };
    let envelope: Envelope = json(&root.join("current.json"))?;
    let manifest: Manifest = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(envelope.manifest)
            .map_err(io::Error::other)?,
    )
    .map_err(io::Error::other)?;
    Ok(
        serde_json::json!({"published_build":manifest.build_id,"managers":rows.into_iter().map(|seen|{
        let age=now().checked_sub(seen.received_at);let current=age.is_some_and(|n|n<=150)&&seen.report.enabled&&seen.report.automatic&&seen.report.error.is_none()&&seen.report.running_build.as_deref()==Some(manifest.build_id.as_str())&&seen.report.installed.as_ref().is_some_and(|r|r.channel==manifest.channel&&r.release==manifest.release&&r.sha256==manifest.sha256);
        serde_json::json!({"address":seen.address,"received_at":seen.received_at,"age_seconds":age,"current":current,"client_reported":seen.report,"display_version":seen.display_version})
    }).collect::<Vec<_>>()}),
    )
}

// The elected update checker owns network work. Presentation only reads this
// bounded, policy-fenced observation; diagnostics confer no update authority.
pub(super) fn refresh(root: &Path, policy: &Policy) {
    let result =
        transport::get_diagnostics(policy.address, "/v1/peers", 128 * 1024).and_then(|bytes| {
            serde_json::from_slice::<serde_json::Value>(&bytes).map_err(io::Error::other)
        });
    let value = match result {
        Ok(peers) => serde_json::json!({"policy":policy,"fetched_at":now(),"peers":peers}),
        Err(error) => {
            serde_json::json!({"policy":policy,"fetched_at":now(),"error":error.to_string()})
        }
    };
    let _ = save(&root.join("peer-status.json"), &value);
}
pub(super) fn cached() -> io::Result<serde_json::Value> {
    let root = root();
    let policy = policy_at(&root)?
        .filter(|p| p.enabled)
        .ok_or_else(|| fail("Automatic updates are off"))?;
    let file = fs::File::open(root.join("peer-status.json"))?;
    let mut bytes = Vec::new();
    file.take(160 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 160 * 1024 {
        return Err(fail("Manager status exceeds size limit"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let cached: Policy =
        serde_json::from_value(value["policy"].clone()).map_err(io::Error::other)?;
    if cached != policy {
        return Err(fail("Waiting for update host status"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_and_old_reports_remain_compatible_and_version_text_is_bounded() {
        let status = serde_json::json!({"version":1,"enabled":true,"automatic":true,"running_build":"build","installed":null,"error":null});
        assert!(decode(&serde_json::to_vec(&status).unwrap(), false)
            .unwrap()
            .1
            .is_none());
        let mut envelope =
            serde_json::json!({"version":2,"status":status,"display_version":"v0.9.5 b2"});
        assert_eq!(
            decode(&serde_json::to_vec(&envelope).unwrap(), true)
                .unwrap()
                .1
                .as_deref(),
            Some("v0.9.5 b2")
        );
        assert!(decode(&serde_json::to_vec(&envelope).unwrap(), false).is_err());
        for version in ["".into(), "x".repeat(65), "bad\nversion".into()] {
            envelope["display_version"] = version.into();
            assert!(decode(&serde_json::to_vec(&envelope).unwrap(), true).is_err());
        }
    }
    #[test]
    fn sender_falls_back_only_when_old_host_explicitly_returns_404() {
        for (response, requests, success) in [
            ("404 Not Found", 2, true),
            ("200 OK", 1, true),
            ("403 Forbidden", 1, false),
            ("500 Internal Server Error", 1, false),
        ] {
            let root = std::env::temp_dir().join(format!(
                "herdr-report-{}",
                crate::client::endpoint::ProfileId::generate()
            ));
            fs::create_dir_all(&root).unwrap();
            save(
                &root.join("status.json"),
                &serde_json::json!({"automatic":true}),
            )
            .unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                for n in 0..requests {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let header =
                        String::from_utf8(transport::headers(&mut stream).unwrap()).unwrap();
                    assert!(header.starts_with(if n == 0 {
                        "POST /v2/report "
                    } else {
                        "POST /v1/report "
                    }));
                    let length = header
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    let body = read_body(&mut stream, length, Duration::from_secs(1)).unwrap();
                    decode(&body, n == 0).unwrap();
                    write!(
                        stream,
                        "HTTP/1.1 {}\r\nContent-Length: 0\r\n\r\n",
                        if n == 0 { response } else { "200 OK" }
                    )
                    .unwrap();
                }
            });
            let policy = Policy {
                version: 1,
                channel: "test".into(),
                address,
                signer: "test".into(),
                enabled: true,
            };
            assert_eq!(send(&root, &policy).is_ok(), success);
            server.join().unwrap();
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn incremental_report_body_has_a_total_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            let _ = s.write_all(b"x");
            std::thread::sleep(Duration::from_millis(200));
        });
        let (mut s, _) = listener.accept().unwrap();
        let start = std::time::Instant::now();
        assert!(read_body(&mut s, 32, Duration::from_millis(30)).is_err());
        assert!(start.elapsed() < Duration::from_millis(150));
        sender.join().unwrap();
    }
    #[test]
    fn diagnostic_reports_are_bounded_and_do_not_accept_invalid_hashes() {
        let mut r = Report {
            version: 1,
            enabled: true,
            automatic: true,
            running_build: Some("build".into()),
            installed: None,
            error: None,
        };
        assert!(valid(&r));
        r.error = Some("x".repeat(513));
        assert!(!valid(&r));
        r.error = None;
        r.installed = Some(Receipt {
            channel: "test".into(),
            release: 1,
            sha256: "../not-a-hash".into(),
            build_id: "build".into(),
        });
        assert!(!valid(&r));
    }
}
