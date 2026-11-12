//! Optional, build-time Linux payloads. No runtime paths or release downloads
//! are needed when the manager was built with `just build-linux-bundle`.

include!(concat!(env!("OUT_DIR"), "/linux_bundle.rs"));

pub(super) fn asset(os: &str, arch: &str) -> Option<&'static [u8]> {
    select(ASSETS, os, arch)
}

fn select<'a>(assets: &[(&str, &'a [u8])], os: &str, arch: &str) -> Option<&'a [u8]> {
    (os == "linux")
        .then(|| assets.iter().find(|(candidate, _)| *candidate == arch))
        .flatten()
        .map(|(_, bytes)| *bytes)
}

pub(super) fn requires_matching_version(os: &str) -> bool {
    os == "linux" && (!ASSETS.is_empty() || crate::build_info::channel() == "local")
}

#[cfg(test)]
mod tests {
    #[test]
    fn selects_only_the_requested_linux_architecture() {
        let assets: &[(&str, &[u8])] = &[("x86_64", b"intel"), ("aarch64", b"arm")];
        assert_eq!(
            super::select(assets, "linux", "x86_64"),
            Some(&b"intel"[..])
        );
        assert_eq!(super::select(assets, "linux", "aarch64"), Some(&b"arm"[..]));
        assert_eq!(super::select(assets, "linux", "riscv64"), None);
        assert_eq!(super::select(assets, "macos", "aarch64"), None);
        assert_eq!(super::select(&[], "linux", "x86_64"), None);
    }
}
