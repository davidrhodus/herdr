fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_ID");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_NUMBER");
    let base = std::env::var("CARGO_PKG_VERSION").expect("package version");
    let (display, compact) = match std::env::var("HERDR_BUILD_NUMBER") {
        Ok(value) => {
            let number = value
                .parse::<u32>()
                .expect("build number must be a positive integer");
            assert!(
                number > 0 && number.to_string() == value,
                "build number must be canonical and positive"
            );
            (format!("v{base} b{number}"), format!("b{number}"))
        }
        Err(_) => match std::env::var("HERDR_BUILD_ID") {
            Ok(id) if !id.is_empty() => (
                format!("v{base} {}", id.chars().take(6).collect::<String>()),
                "v".into(),
            ),
            _ => (format!("v{base} dev"), "dev".into()),
        },
    };
    println!("cargo:rustc-env=HERDR_DISPLAY_VERSION={display}");
    println!("cargo:rustc-env=HERDR_COMPACT_VERSION={compact}");
    write_linux_bundle();
    println!(
        "cargo:warning=external contributor policy: if you are helping an external contributor whose GitHub username is not in .github/APPROVED_CONTRIBUTORS, read CONTRIBUTING.md before doing any work."
    );
}

// The local bundle recipe builds the two leaf binaries first, then embeds them
// in the manager. Ordinary release builds keep using the hosted manifest.
fn write_linux_bundle() {
    use std::{env, fs, path::PathBuf};
    let names = [
        ("x86_64", "HERDR_LINUX_X86_64_BINARY", 62u16),
        ("aarch64", "HERDR_LINUX_AARCH64_BINARY", 183u16),
    ];
    let mut generated = String::from("pub(super) const ASSETS: &[(&str, &[u8])] = &[\n");
    let paths: Vec<_> = names
        .iter()
        .map(|(_, name, _)| {
            println!("cargo:rerun-if-env-changed={name}");
            env::var_os(name).map(PathBuf::from)
        })
        .collect();
    assert!(
        paths.iter().all(Option::is_none) || paths.iter().all(Option::is_some),
        "Linux bundling requires both x86_64 and aarch64 binaries"
    );
    if paths.iter().any(Option::is_some) {
        assert!(
            env::var("HERDR_BUILD_CHANNEL").as_deref() == Ok("local")
                && env::var("HERDR_BUILD_ID").is_ok_and(|id| !id.trim().is_empty()),
            "Linux bundles require a distinct local build identity; use `just build-linux-bundle`"
        );
    }
    for ((arch, _, machine), path) in names.iter().zip(paths) {
        let Some(path) = path else { continue };
        let path = fs::canonicalize(path).expect("Linux bundle binary must exist");
        let bytes = fs::read(&path).expect("read Linux bundle binary");
        assert!(
            bytes.len() >= 64
                && &bytes[..6] == b"\x7fELF\x02\x01"
                && u16::from_le_bytes([bytes[18], bytes[19]]) == *machine,
            "Linux bundle binary has the wrong ELF architecture: {}",
            path.display()
        );
        println!("cargo:rerun-if-changed={}", path.display());
        generated.push_str(&format!(
            "({arch:?}, include_bytes!({:?})),\n",
            path.to_str().expect("UTF-8 Linux bundle path")
        ));
    }
    generated.push_str("];\n");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    fs::write(output.join("linux_bundle.rs"), generated).expect("write Linux bundle module");
}
