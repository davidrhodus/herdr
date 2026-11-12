#![cfg(unix)]

use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

struct Cleanup(PathBuf);
impl Cleanup {
    fn stop(&self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(["server", "stop"])
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_CLIENT_SOCKET_PATH")
            .env_remove("HERDR_SESSION")
            .output();
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.0);
    }
}

// Exercise the actual interactive CLI used by the TUI, with a stopped remote
// server and a fake SSH transport. No real hosts or credentials are used.
#[test]
fn reconnect_and_update_prepare_stopped_saved_servers_without_rewriting_profiles() {
    let root = PathBuf::from("/tmp").join(format!("hr-repair-{}", std::process::id()));
    let cleanup = Cleanup(root.clone());
    fs::create_dir_all(root.join("bin")).unwrap();
    let app = if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    };
    let state = root.join("state").join(app).join("client");
    let config = root.join("config").join(app);
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        "[remote]\nmanage_ssh_config=false\n",
    )
    .unwrap();
    let catalog = br#"{"version":1,"ssh":[{"id":"3655d8ad01dacaf1fd70118dd207d751","label":"Redie","target":"david@repair.invalid","session":"default","enabled":true}]}"#;
    fs::write(state.join("endpoints.json"), catalog).unwrap();
    let script = r#"#!/bin/sh
for arg do last=$arg; done
case "$last" in *powershell*) exit 1 ;; exit) exit 0 ;; esac
if [ "$last" = '/bin/sh -s' ]; then input=$(cat); else input=$last; fi
printf '%s\n' "$input" >> "$HERDR_REPAIR_ROOT/commands"
printf '\nherdr-remote-output-ready:1\n'
case "$input" in
    *'uname -s'*) printf 'Linux\nx86_64\n' ;;
    *'status client --json'*) printf '{"version":"%s","endpoint_protocol_generation":1,"endpoint_capabilities":["surface_interest","presentation_effects_fence","health_check"]}\n' "$HERDR_REPAIR_VERSION" ;;
    *'status server --json'*)
        if [ -e "$HERDR_REPAIR_ROOT/force-old" ]; then
            "$HERDR_REPAIR_BINARY" status server --json | sed 's/"version": *"[^"]*"/"version":"0.0.0"/'
        else exec "$HERDR_REPAIR_BINARY" status server --json; fi ;;
    *'server stop'*) rm -f "$HERDR_REPAIR_ROOT/force-old"; touch "$HERDR_REPAIR_ROOT/stopped"; exec "$HERDR_REPAIR_BINARY" server stop ;;
    *remote-client-bridge*) touch "$HERDR_REPAIR_ROOT/started"; exec "$HERDR_REPAIR_BINARY" remote-client-bridge ;;
    *'command -v herdr'*|*'emit()'*) printf '/srv/herdr\n' ;;
    *) exit 1 ;;
esac
"#;
    let ssh = root.join("bin/ssh");
    fs::write(&ssh, script).unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    let version = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("--version")
        .output()
        .unwrap();
    let version = String::from_utf8(version.stdout)
        .unwrap()
        .trim()
        .strip_prefix("herdr ")
        .unwrap()
        .to_owned();
    for (action, approval) in [
        ("reconnect", None),
        ("update", None),
        ("update", Some(false)),
        ("update", Some(true)),
    ] {
        if approval.is_none() {
            cleanup.stop();
        } else {
            fs::write(root.join("force-old"), b"yes").unwrap();
        }
        let _ = fs::remove_file(root.join("stopped"));
        let _ = fs::remove_file(root.join("started"));
        let pty = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
        command.args(["machine", action, "Redie"]);
        command.env(
            "PATH",
            format!(
                "{}:{}",
                root.join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        command.env("XDG_STATE_HOME", root.join("state"));
        command.env("XDG_CONFIG_HOME", root.join("config"));
        command.env("HERDR_REPAIR_ROOT", &root);
        command.env("HERDR_REPAIR_VERSION", &version);
        command.env("HERDR_REPAIR_BINARY", env!("CARGO_BIN_EXE_herdr"));
        command.env("SHELL", "/bin/sh");
        for key in [
            "HERDR_CONFIG_PATH",
            "HERDR_SOCKET_PATH",
            "HERDR_CLIENT_SOCKET_PATH",
            "HERDR_SESSION",
            "HERDR_REMOTE_BINARY",
            "HERDR_SETUP_PROGRESS_FILE",
        ] {
            command.env_remove(key);
        }
        let mut child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().unwrap();
        let mut writer = pty.master.take_writer().unwrap();
        let output = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            let mut answered = false;
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(accepted) = approval {
                    if !answered
                        && String::from_utf8_lossy(&bytes)
                            .contains("restart the remote server now?")
                    {
                        writer
                            .write_all(if accepted { b"y\n" } else { b"n\n" })
                            .unwrap();
                        answered = true;
                    }
                }
            }
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("{action} timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = String::from_utf8_lossy(&output.join().unwrap()).into_owned();
        if approval == Some(false) {
            assert!(!status.success(), "declined restart reported success");
            assert!(
                output.contains("running server update cancelled"),
                "{output}"
            );
            assert!(!root.join("stopped").exists());
        } else {
            assert!(status.success(), "{action}: {output}");
        }
        if approval == Some(true) {
            assert!(root.join("stopped").exists());
        }
        assert!(
            root.join("started").exists(),
            "{action} did not restart the server"
        );
        assert_eq!(fs::read(state.join("endpoints.json")).unwrap(), catalog);
    }
    drop(cleanup);
}
