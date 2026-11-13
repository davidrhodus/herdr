//! Direct HTTP over the pinned Tailscale address: no proxies or redirects.
use super::*;
use std::net::{SocketAddr, TcpStream};
use std::time::Instant;
pub(super) fn headers(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut header = Vec::new();
    let mut byte = [0];
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 4096 || Instant::now() > deadline {
            return Err(fail("HTTP header limit exceeded"));
        }
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
    }
    Ok(header)
}
fn request(address: SocketAddr, path: &str, limit: u64) -> io::Result<(TcpStream, u64)> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(8))?;
    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
    stream.set_write_timeout(Some(Duration::from_secs(8)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    let raw = headers(&mut stream)?;
    let header = std::str::from_utf8(&raw).map_err(io::Error::other)?;
    let mut lines = header.split("\r\n");
    if lines.next() != Some("HTTP/1.1 200 OK") {
        return Err(fail("Private update service did not return a release"));
    }
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| fail("Invalid update HTTP headers"))?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(fail("Unexpected chunked update"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(fail("Duplicate update body length"));
            }
            length = Some(value.trim().parse::<u64>().map_err(io::Error::other)?);
        }
    }
    let length = length
        .filter(|n| *n > 0 && *n <= limit)
        .ok_or_else(|| fail("Invalid update body length"))?;
    Ok((stream, length))
}
fn body(mut stream: TcpStream, length: u64, output: &mut impl Write) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut remaining = length;
    let mut bytes = [0u8; 65536];
    while remaining > 0 {
        if Instant::now() > deadline {
            return Err(fail("Update download deadline exceeded"));
        }
        let n = stream.read(&mut bytes[..remaining.min(65536) as usize])?;
        if n == 0 {
            return Err(fail("Truncated private update download"));
        }
        output.write_all(&bytes[..n])?;
        remaining -= n as u64;
    }
    Ok(())
}
pub(super) fn get(address: SocketAddr, path: &str, limit: u64) -> io::Result<Vec<u8>> {
    let (stream, length) = request(address, path, limit)?;
    let mut bytes = Vec::new();
    body(stream, length, &mut bytes)?;
    Ok(bytes)
}
pub(super) fn download(
    address: SocketAddr,
    path: &str,
    expected: u64,
    file: &mut fs::File,
) -> io::Result<()> {
    let (stream, length) = request(address, path, expected)?;
    if length != expected {
        return Err(fail("Update length differs from signed manifest"));
    }
    body(stream, length, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn server(response: Vec<u8>) -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let _ = headers(&mut stream);
            let _ = stream.write_all(&response);
        });
        addr
    }
    #[test]
    fn redirects_chunked_duplicate_lengths_oversized_and_truncated_bodies_are_rejected() {
        for bytes in [
            b"HTTP/1.1 302 Found\r\nContent-Length: 1\r\n\r\nx".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 1\r\n\r\nx".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\nx".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nx".to_vec(),
        ] {
            assert!(get(server(bytes), "/v1/current", 100).is_err());
        }
        assert_eq!(
            get(
                server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec()),
                "/v1/current",
                100
            )
            .unwrap(),
            b"ok"
        );
    }
}
