use super::*;
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn copy(input: &mut fs::File, destination: &Path) -> io::Result<()> {
    if let Ok(meta) = fs::symlink_metadata(destination) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(fail("Refusing to replace non-file update destination"));
        }
    }
    let parent = destination
        .parent()
        .ok_or_else(|| fail("Missing install directory"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".herdr-install-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = crate::platform::create_private_state_file(&temp)?;
        io::copy(input, &mut file)?;
        file.sync_all()?;
        drop(file);
        crate::platform::private_updates::make_executable(&temp)?;
        crate::platform::replace_file(&temp, destination)?;
        crate::platform::sync_parent_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
pub(super) fn promote(source: &Path, destination: &Path) -> io::Result<()> {
    bounded_metadata(source)?;
    if fs::canonicalize(source).ok() == fs::canonicalize(destination).ok() {
        return Ok(());
    }
    let mut input = fs::File::open(source)?;
    if destination.exists() {
        bounded_metadata(destination)?;
        copy(
            &mut fs::File::open(destination)?,
            &destination.with_file_name("herdr.previous"),
        )?;
    }
    copy(&mut input, destination)
}
