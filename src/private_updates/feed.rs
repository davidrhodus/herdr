//! Read-only immutable artifact service, bound exclusively to this host's Tailscale IP.
use super::*;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::AtomicUsize;
use std::time::Instant;
fn path(root: &Path, request: &str) -> Option<PathBuf> {
    match request {
        "/v1/current" => Some(root.join("current.json")),
        _ => request
            .strip_prefix("/v1/binary/")
            .filter(|hash| valid_hash(hash))
            .map(|hash| root.join("artifacts").join(hash)),
    }
}
fn owned(address: SocketAddr) -> io::Result<()> {
    if !crate::client::tailscale::tailscale_ip(address.ip())
        || !crate::client::tailscale::network()
            .map_err(fail)?
            .own
            .ips
            .contains(&address.ip())
    {
        return Err(fail(
            "Update feed must bind this computer's current Tailscale address",
        ));
    }
    Ok(())
}
fn connection(root: &Path, mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let header = transport::headers(&mut stream)?;
    let header = std::str::from_utf8(&header).map_err(io::Error::other)?;
    let first = header.split("\r\n").next().unwrap_or("");
    let parts: Vec<_> = first.split(' ').collect();
    if parts.as_slice() == ["POST", "/v1/report", "HTTP/1.1"]
        || parts.as_slice() == ["POST", "/v2/report", "HTTP/1.1"]
    {
        return report::receive(
            root,
            stream.peer_addr()?,
            header,
            &mut stream,
            parts[1] == "/v2/report",
        );
    }
    if parts.as_slice() == ["GET", "/v1/peers", "HTTP/1.1"] {
        if !crate::client::tailscale::tailscale_ip(stream.peer_addr()?.ip()) {
            return Err(fail("Manager status requires Tailscale"));
        }
        let bytes = serde_json::to_vec(&report::peers_at(root)?).map_err(io::Error::other)?;
        if bytes.len() > 128 * 1024 {
            return Err(fail("Manager status exceeds size limit"));
        }
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", bytes.len())?;
        return stream.write_all(&bytes);
    }
    let file = if parts.len() == 3 && parts[0] == "GET" && parts[2] == "HTTP/1.1" {
        path(root, parts[1])
    } else {
        None
    };
    let Some(file) = file else {
        stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        return Ok(());
    };
    let limit = if parts[1] == "/v1/current" {
        MAX_ENVELOPE
    } else {
        MAX_BINARY
    };
    let meta = fs::symlink_metadata(&file)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > limit {
        return Err(fail("Invalid update artifact"));
    }
    let mut input = fs::File::open(file)?;
    let length = input.metadata()?.len();
    write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n")?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut bytes = [0u8; 65536];
    let mut remaining = length;
    while remaining > 0 {
        if Instant::now() > deadline {
            return Err(fail("Update serving deadline exceeded"));
        }
        let n = input.read(&mut bytes[..remaining.min(65536) as usize])?;
        if n == 0 {
            return Err(fail("Artifact changed during transfer"));
        }
        stream.write_all(&bytes[..n])?;
        remaining -= n as u64;
    }
    Ok(())
}
pub(super) fn serve(address: SocketAddr) -> io::Result<()> {
    owned(address)?;
    let root = crate::config::state_dir().join("private-update-feed");
    fs::create_dir_all(root.join("artifacts"))?;
    let listener = TcpListener::bind(address)?;
    listener.set_nonblocking(true)?;
    let active = Arc::new(AtomicUsize::new(0));
    let mut verified = Instant::now();
    loop {
        if verified.elapsed() > Duration::from_secs(15) {
            owned(address)?;
            verified = Instant::now();
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if active.load(Ordering::Acquire) >= 4 {
                    drop(stream);
                    continue;
                }
                active.fetch_add(1, Ordering::AcqRel);
                let active = active.clone();
                let root = root.clone();
                std::thread::spawn(move || {
                    let _ = connection(&root, stream);
                    active.fetch_sub(1, Ordering::AcqRel);
                });
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100))
            }
            Err(e) => return Err(e),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_are_exact_and_cannot_read_other_files() {
        let root = Path::new("/feed");
        assert!(path(root, "/v1/current").is_some());
        assert!(path(root, &format!("/v1/binary/{}", "a".repeat(64))).is_some());
        for p in [
            "/v1/current?x",
            "/v1/current/",
            "/v1/binary/../../.ssh/id_ed25519",
            "/v1/binary/%2e%2e",
            "/v1/binary/AAAA",
            "/etc/passwd",
        ] {
            assert!(path(root, p).is_none());
        }
    }
}
