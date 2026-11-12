#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

// Use the real Herdr executable for discovery and exchange, with SSH's command
// transport replaced by a local shell and a separate peer state directory.
const SSH: &str = r#"#!/bin/sh
for arg do last=$arg; printf '%s\n' "$arg" >> "$HERDR_TEST_ROOT/ssh-args"; done
if [ "$HERDR_TEST_OFFLINE" = yes ]; then echo 'peer offline' >&2; exit 255; fi
export XDG_STATE_HOME="$HERDR_TEST_PEER_STATE"
export XDG_CONFIG_HOME="$HERDR_TEST_PEER_CONFIG"
PATH="$HERDR_TEST_REMOTE_BIN:/usr/bin:/bin"; export PATH
printf 'login banner\n'
if [ "$last" = '/bin/sh -s' ]; then
    script=$(cat)
    exec /bin/sh -c "$script"
fi
exec /bin/sh -c "$last"
"#;

struct Harness {
    root: PathBuf,
}
impl Harness {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "herdr-peer-sync-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("remote bin")).unwrap();
        fs::write(root.join("bin/ssh"), SSH).unwrap();
        fs::set_permissions(root.join("bin/ssh"), fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_herdr"), root.join("remote bin/herdr"))
            .unwrap();
        for device in ["host", "laptop"] {
            let config = root.join(format!("{device}-config")).join(app_name());
            fs::create_dir_all(&config).unwrap();
            fs::write(
                config.join("config.toml"),
                "[remote]\nmanage_ssh_config = false\n",
            )
            .unwrap();
        }
        Self { root }
    }
    fn state(&self, device: &str) -> PathBuf {
        self.root.join(format!("{device}-state"))
    }
    fn catalog(&self, device: &str) -> PathBuf {
        self.state(device)
            .join(app_name())
            .join("client/endpoints.json")
    }
    fn command(&self, device: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
        command
            .env("XDG_STATE_HOME", self.state(device))
            .env(
                "XDG_CONFIG_HOME",
                self.root.join(format!("{device}-config")),
            )
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("HERDR_TEST_ROOT", &self.root)
            .env("HERDR_TEST_PEER_STATE", self.state("host"))
            .env("HERDR_TEST_PEER_CONFIG", self.root.join("host-config"))
            .env("HERDR_TEST_REMOTE_BIN", self.root.join("remote bin"))
            .env_remove("HERDR_CONFIG_PATH")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_CLIENT_SOCKET_PATH")
            .env_remove("HERDR_SESSION");
        command
    }
    fn run(&self, device: &str, args: &[&str]) -> Output {
        let output = self.command(device).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn seed(&self, device: &str, id: &str, label: &str) {
        let path = self.catalog(device);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec(&json!({"version":1,"ssh":[{"id":id,"label":label,"target":"build.tailnet.ts.net","session":"main","enabled":true}]})).unwrap()).unwrap();
    }
    fn profiles(&self, device: &str) -> Value {
        serde_json::from_slice::<Value>(&fs::read(self.catalog(device)).unwrap()).unwrap()["ssh"]
            .clone()
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn app_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

#[test]
fn real_cli_peers_merge_offline_edits_and_deletions_through_ssh_stdio() {
    let harness = Harness::new();
    let a = "a".repeat(32);
    let b = "b".repeat(32);
    harness.seed("host", &a, "Desktop");
    harness.seed("laptop", &b, "Laptop");
    harness.run("host", &["sync", "host"]);
    harness.run("laptop", &["sync", "join", "you@desktop.tailnet.ts.net"]);
    assert_eq!(harness.profiles("host"), harness.profiles("laptop"));
    assert_eq!(harness.profiles("host").as_array().unwrap().len(), 2);
    // Rename locally while disconnected: the command must still succeed.
    let offline = harness
        .command("laptop")
        .env("HERDR_TEST_OFFLINE", "yes")
        .args(["machine", "rename", &a, "--label", "Offline rename"])
        .output()
        .unwrap();
    assert!(
        offline.status.success(),
        "{}",
        String::from_utf8_lossy(&offline.stderr)
    );
    harness.run("host", &["machine", "remove", &b]);
    harness.run("laptop", &["sync", "now"]);
    assert_eq!(harness.profiles("host"), harness.profiles("laptop"));
    assert_eq!(harness.profiles("host").as_array().unwrap().len(), 1);
    assert_eq!(harness.profiles("host")[0]["label"], "Offline rename");
    let arguments = fs::read_to_string(harness.root.join("ssh-args")).unwrap();
    assert!(arguments.contains("BatchMode=yes"));
    assert!(arguments.contains("StrictHostKeyChecking=yes"));
    assert!(!arguments.contains("StrictHostKeyChecking=no"));
    harness.run("laptop", &["sync", "leave"]);
    assert_eq!(harness.profiles("laptop").as_array().unwrap().len(), 1);
    let status = harness.run("laptop", &["sync", "status"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap()["enabled"],
        false
    );
}

#[test]
fn unconfigured_host_refuses_join_and_preserves_the_local_list() {
    let harness = Harness::new();
    harness.seed("laptop", &"b".repeat(32), "Laptop");
    let output = harness
        .command("laptop")
        .args(["sync", "join", "you@desktop"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(harness.profiles("laptop").as_array().unwrap().len(), 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("herdr sync host"));
    // Configuration remains available for retry once the peer enables hosting.
    harness.run("host", &["sync", "host"]);
    harness.run("laptop", &["sync", "now"]);
    assert_eq!(harness.profiles("host"), harness.profiles("laptop"));
}

#[test]
fn corrupt_sync_metadata_does_not_block_local_machine_changes_or_leaving() {
    let harness = Harness::new();
    let id = "a".repeat(32);
    harness.seed("laptop", &id, "Before");
    let state = harness
        .catalog("laptop")
        .with_file_name("catalog-sync.json");
    fs::write(&state, b"invalid sync metadata").unwrap();
    harness.run("laptop", &["machine", "rename", &id, "--label", "After"]);
    assert_eq!(harness.profiles("laptop")[0]["label"], "After");
    harness.run("laptop", &["sync", "leave"]);
    assert!(!state.exists());
    assert_eq!(harness.profiles("laptop")[0]["label"], "After");
}

#[test]
fn choosing_a_saved_server_prepares_the_shared_host_without_manual_remote_commands() {
    let harness = Harness::new();
    harness.seed("host", &"a".repeat(32), "Host");
    harness.seed("laptop", &"b".repeat(32), "Laptop");
    harness.run("laptop", &["sync", "use", "you@desktop"]);
    assert_eq!(harness.profiles("host"), harness.profiles("laptop"));
    assert_eq!(harness.profiles("host").as_array().unwrap().len(), 2);
}
