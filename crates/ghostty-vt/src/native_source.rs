//! Bounded local Kitty image media and platform-specific source retention.
#[cfg(target_os = "linux")]
use crate::pane_graphics_files::FileStore;
use crate::pane_graphics_files::OwnedExport;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    sync::Arc,
};
const LIMIT: usize = 64 * 1024 * 1024;
#[cfg(target_os = "linux")]
static SOURCES: OnceLock<FileStore> = OnceLock::new();
pub(crate) fn load(
    path: &str,
    medium: u8,
    offset: u64,
    size: Option<usize>,
    forward: bool,
) -> io::Result<(Vec<u8>, Option<Arc<OwnedExport>>)> {
    let mut file = if medium == b's' {
        shared_memory(path)?
    } else {
        open_regular(path)?
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("image source must be a regular file"));
    }
    let len = metadata.len();
    let expected =
        size.unwrap_or_else(|| len.saturating_sub(offset).min((LIMIT + 1) as u64) as usize);
    if expected > LIMIT || expected as u64 > len.saturating_sub(offset) {
        return Err(io::Error::other("image exceeds storage limit"));
    }
    let retained = if forward && offset == 0 && medium != b's' {
        snapshot(&file, expected)
    } else {
        None
    };
    if retained.is_some() {
        return Ok((Vec::new(), retained));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::with_capacity(expected);
    file.take(expected as u64).read_to_end(&mut bytes)?;
    if bytes.len() != expected {
        return Err(io::Error::other("truncated image source"));
    }
    if medium == b't' {
        let file = std::path::Path::new(path);
        if file.starts_with(std::env::temp_dir())
            && file
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains("tty-graphics-protocol"))
        {
            let _ = std::fs::remove_file(file);
        }
    }
    Ok((bytes, retained))
}
#[cfg(target_os = "linux")]
fn snapshot(file: &File, len: usize) -> Option<Arc<OwnedExport>> {
    use std::os::fd::AsRawFd;
    SOURCES
        .get_or_init(FileStore::native_sources)
        .snapshot(i64::from(file.as_raw_fd()), len)
        .ok()
        .map(Arc::new)
}
#[cfg(not(target_os = "linux"))]
fn snapshot(_file: &File, _len: usize) -> Option<Arc<OwnedExport>> {
    None
}
#[cfg(unix)]
fn shared_memory(name: &str) -> io::Result<File> {
    use std::{ffi::CString, os::fd::FromRawFd};
    let name = CString::new(name).map_err(|_| io::Error::other("invalid shared-memory name"))?;
    let fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
#[cfg(not(unix))]
fn shared_memory(_name: &str) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "shared-memory image media unsupported",
    ))
}

#[cfg(unix)]
fn open_regular(path: &str) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}
#[cfg(not(unix))]
fn open_regular(path: &str) -> io::Result<File> {
    File::open(path)
}
