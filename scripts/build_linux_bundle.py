"""Build a local manager with matching, static Linux x86_64 and ARM64 payloads.

Uses Rust's musl targets and bundled LLD linker; no Docker, Zig, Linux host,
remote compiler, or published Herdr release is required.
"""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
TARGETS = {
    "x86_64": "x86_64-unknown-linux-musl",
    "aarch64": "aarch64-unknown-linux-musl",
}
BUNDLE_VARIABLES = ("HERDR_LINUX_X86_64_BINARY", "HERDR_LINUX_AARCH64_BINARY")


def source_id(root: Path) -> str:
    # Include dirty/untracked Rust source as well as vendored dependencies.
    # Exclude generated target output, local configuration, and git metadata.
    paths = {root / name for name in ("Cargo.toml", "Cargo.lock", "build.rs")}
    for name in ("src", "crates", "vendor", "assets", ".cargo"):
        paths.update(path for path in (root / name).rglob("*") if path.is_file())
    paths.add(root / "scripts/build_linux_bundle.py")
    digest = hashlib.sha256()
    for path in sorted(paths):
        relative = path.relative_to(root)
        if any(part in ("target", ".git", "__pycache__") for part in relative.parts):
            continue
        name = relative.as_posix().encode()
        data = path.read_bytes()
        for value in (name, data):
            digest.update(len(value).to_bytes(8, "big"))
            digest.update(value)
    return digest.hexdigest()[:24]


def leaf_environment(environment: dict, identity: str) -> dict:
    result = environment.copy()
    for variable in BUNDLE_VARIABLES:
        result.pop(variable, None)
    result["HERDR_BUILD_CHANNEL"] = "local"
    result["HERDR_BUILD_ID"] = identity
    result["CARGO_PROFILE_RELEASE_STRIP"] = "symbols"
    return result


def build() -> None:
    identity = source_id(ROOT)
    output = ROOT / "target/linux-bundle" / identity
    output.mkdir(parents=True, exist_ok=True)
    environment = leaf_environment(os.environ, identity)
    # Pin the target directory so payload discovery never depends on a user's
    # CARGO_TARGET_DIR setting. Keep leaf and manager outputs separate.
    environment["CARGO_TARGET_DIR"] = str(ROOT / "target/linux-bundle-build")
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    host = next(
        line.removeprefix("host: ")
        for line in subprocess.check_output(["rustc", "-vV"], text=True).splitlines()
        if line.startswith("host: ")
    )
    linker = sysroot / "lib/rustlib" / host / "bin/rust-lld"
    if not linker.is_file():
        raise ValueError(f"Rust's bundled linker is missing: {linker}")
    subprocess.run(["rustup", "target", "add", *TARGETS.values()], cwd=ROOT, check=True)
    manifest = {"source_id": identity, "channel": "local", "assets": {}}
    for arch, target in TARGETS.items():
        variable = "CARGO_TARGET_" + target.upper().replace("-", "_") + "_LINKER"
        leaf = environment | {variable: str(linker)}
        subprocess.run(
            ["cargo", "build", "--release", "--locked", "--bin", "herdr", "--target", target],
            cwd=ROOT, env=leaf, check=True,
        )
        binary = output / f"herdr-linux-{arch}"
        shutil.copy2(Path(environment["CARGO_TARGET_DIR"]) / target / "release/herdr", binary)
        manifest["assets"][arch] = {
            "file": binary.name,
            "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        }
    if source_id(ROOT) != identity:
        raise ValueError("Source changed during the Linux builds; rerun to build a matching bundle")
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    manager = environment | {
        variable: str(output / f"herdr-linux-{arch}")
        for variable, arch in zip(BUNDLE_VARIABLES, TARGETS)
    }
    manager["CARGO_TARGET_DIR"] = str(ROOT / "target")
    subprocess.run(
        ["cargo", "build", "--release", "--locked", "--bin", "herdr"],
        cwd=ROOT, env=manager, check=True,
    )
    if source_id(ROOT) != identity:
        raise ValueError("Source changed during the manager build; rerun before installing")
    print(f"Built target/release/herdr with Linux x86_64 + ARM64 ({identity})", flush=True)
    print(f"Payload checksums: {output / 'manifest.json'}", flush=True)


def main() -> int:
    try:
        build()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
