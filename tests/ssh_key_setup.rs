#![cfg(unix)]

use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(["server", "stop"])
            .env("XDG_STATE_HOME", self.0.join("remote-state"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_CLIENT_SOCKET_PATH")
            .env_remove("HERDR_SESSION")
            .output();
        let _ = fs::remove_dir_all(&self.0);
    }
}

// The transport is fake; key generation and authorized_keys editing are real.
// The only password is a disposable fixture entered through a private PTY.
const SSH: &str = r#"#!/usr/bin/env python3
import base64,hashlib,os,pathlib,subprocess,sys,termios
root=pathlib.Path(os.environ['HERDR_KEY_ROOT'])
args=sys.argv[1:]
authorized=root/'remote/.ssh/authorized_keys'
if '-v' in args:
    for option in ['BatchMode=yes','ControlPath=none','ControlMaster=no','IdentityAgent=none','PasswordAuthentication=no','KbdInteractiveAuthentication=no','PreferredAuthentications=publickey']:
        assert option in args,option
    key=args[args.index('-i')+1]
    public=subprocess.check_output(['ssh-keygen','-y','-P','','-f',key]).decode().split()
    if os.environ.get('HERDR_KEY_VERIFY_FAIL')=='yes' or not authorized.exists() or not any(line.split()[:2]==public[:2] for line in authorized.read_text().splitlines()):
        sys.exit(255)
    fingerprint='SHA256:'+base64.b64encode(hashlib.sha256(base64.b64decode(public[1])).digest()).decode().rstrip('=')
    print('debug1: Server accepts key: '+key+' ED25519 '+fingerprint+' explicit',file=sys.stderr)
    print('Authenticated to test ([127.0.0.1]:22) using "publickey".',file=sys.stderr)
    sys.exit(0)
if not (args[-1].startswith('/bin/sh -c ') and 'Herdr public key installed.' in args[-1]):
    if 'powershell' in args[-1]:sys.exit(1)
    assert '-i' in args,'setup probe did not use the enrolled key'
    public=subprocess.check_output(['ssh-keygen','-y','-P','','-f',args[args.index('-i')+1]]).decode().split()
    assert authorized.exists() and any(line.split()[:2]==public[:2] for line in authorized.read_text().splitlines()),'setup probe used a different key'
    command=sys.stdin.read() if args[-1]=='/bin/sh -s' else args[-1]
    if 'base64 --decode' in command:command=base64.b64decode(command.split('echo ',1)[1].split(' |',1)[0]).decode()
    print('\nherdr-remote-output-ready:1',flush=True)
    environment=os.environ.copy();environment['XDG_STATE_HOME']=str(root/'remote-state')
    binary=os.environ['HERDR_KEY_BINARY']
    if 'uname -s' in command:print('Linux\nx86_64');sys.exit(0)
    if 'command -v herdr' in command or 'emit()' in command:print('/srv/herdr');sys.exit(0)
    if 'session list --json' in command:print('{"sessions":[]}');sys.exit(0)
    if 'status client --json' in command:sys.exit(subprocess.call([binary,'status','client','--json'],env=environment))
    if 'status server --json' in command:sys.exit(subprocess.call([binary,'status','server','--json'],env=environment))
    if 'remote-api-bridge' in command:sys.exit(subprocess.call([binary,'remote-api-bridge']+(['--check'] if '--check' in command and 'exec ' not in command else []),env=environment))
    if 'remote-client-bridge' in command:sys.exit(subprocess.call([binary,'remote-client-bridge'],env=environment))
    raise AssertionError('Unexpected setup probe: '+command)
fd=os.open('/dev/tty',os.O_RDWR)
previous=termios.tcgetattr(fd);settings=termios.tcgetattr(fd);settings[3]&=~termios.ECHO
try:
    termios.tcsetattr(fd,termios.TCSANOW,settings)
    os.write(fd,b'Fixture SSH password: ')
    answer=b''
    while not answer.endswith(b'\n'):answer+=os.read(fd,1)
    assert answer.strip()==b'fixture-only-password'
finally:
    termios.tcsetattr(fd,termios.TCSANOW,previous);os.write(fd,b'\n');os.close(fd)
with (root/'install-count').open('a') as log:log.write('install\n')
command=args[-1].replace('$HOME/.ssh','$HERDR_KEY_REMOTE/.ssh')
sys.exit(subprocess.call(['/bin/sh','-c',command]))
"#;

#[test]
fn enrollment_is_automatic_retryable_and_saves_only_verified_local_identity() {
    let root = PathBuf::from("/tmp").join(format!("hr-key-{}", std::process::id()));
    let _cleanup = Cleanup(root.clone());
    let app = if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    };
    let state = root.join("state").join(app).join("client");
    let config = root.join("config").join(app);
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(root.join("remote")).unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(
        config.join("config.toml"),
        "[remote]\nmanage_ssh_config=false\n",
    )
    .unwrap();
    let catalog = json!({"version":1,"ssh":[
        {"id":"3655d8ad01dacaf1fd70118dd207d751","label":"Redie","target":"david@key-test.invalid","session":"default","enabled":true},
        {"id":"4655d8ad01dacaf1fd70118dd207d751","label":"Failure","target":"david@fail-test.invalid","session":"default","enabled":true}
    ]});
    let catalog = serde_json::to_vec(&catalog).unwrap();
    fs::write(state.join("endpoints.json"), &catalog).unwrap();
    let ssh = root.join("bin/ssh");
    fs::write(&ssh, SSH).unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    let identity = state.join("ssh-identities.json");
    for (name, fail, prompts) in [
        ("Redie", false, true),
        ("Redie", false, false),
        ("Failure", true, true),
        ("Failure", false, false),
        ("Added", false, true),
    ] {
        if name == "Failure" && fail {
            let mut settings: Value =
                serde_json::from_slice(&fs::read(&identity).unwrap()).unwrap();
            settings["david@fail-test.invalid"] = json!("/existing/device-key");
            fs::write(&identity, serde_json::to_vec(&settings).unwrap()).unwrap();
        }
        let pty = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 30,
                cols: 110,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
        if name == "Added" {
            command.args([
                "machine",
                "add",
                "david@add-test.invalid",
                "--label",
                "Added",
                "--setup-key",
            ]);
        } else {
            command.args(["machine", "setup-key", name]);
        }
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
        command.env("HERDR_KEY_ROOT", &root);
        command.env("HERDR_KEY_BINARY", env!("CARGO_BIN_EXE_herdr"));
        command.env("HERDR_KEY_REMOTE", root.join("remote"));
        command.env("HERDR_KEY_VERIFY_FAIL", if fail { "yes" } else { "no" });
        for key in [
            "HERDR_CONFIG_PATH",
            "HERDR_SOCKET_PATH",
            "HERDR_CLIENT_SOCKET_PATH",
            "HERDR_SESSION",
            "HERDR_SETUP_PROGRESS_FILE",
            "HERDR_SETUP_IDENTITY_TARGET",
            "HERDR_SETUP_IDENTITY_PATH",
        ] {
            command.env_remove(key);
        }
        if name == "Added" {
            command.env("HERDR_SETUP_IDENTITY_TARGET", "david@add-test.invalid");
            command.env("HERDR_SETUP_IDENTITY_PATH", "/existing/bootstrap-key");
        }
        let mut child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().unwrap();
        let mut writer = pty.master.take_writer().unwrap();
        let output = std::thread::spawn(move || {
            let mut text = Vec::new();
            let mut buffer = [0u8; 4096];
            let mut answered = false;
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                text.extend_from_slice(&buffer[..count]);
                if !answered && String::from_utf8_lossy(&text).contains("Fixture SSH password:") {
                    writer.write_all(b"fixture-only-password\n").unwrap();
                    answered = true;
                }
            }
            (text, answered)
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("key setup timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let (bytes, answered) = output.join().unwrap();
        let output = String::from_utf8_lossy(&bytes);
        assert_eq!(answered, prompts, "{output}");
        assert_eq!(status.success(), !fail, "{output}");
        assert!(
            !output.contains("fixture-only-password"),
            "password was echoed"
        );
        if name == "Added" {
            let saved: Value =
                serde_json::from_slice(&fs::read(state.join("endpoints.json")).unwrap()).unwrap();
            assert!(saved["ssh"]
                .as_array()
                .unwrap()
                .iter()
                .any(|profile| profile["label"] == "Added"
                    && profile["target"] == "david@add-test.invalid"
                    && profile["session"] == "default"));
        } else {
            assert_eq!(fs::read(state.join("endpoints.json")).unwrap(), catalog);
        }
        let settings: Value = serde_json::from_slice(&fs::read(&identity).unwrap()).unwrap();
        if fail {
            assert_eq!(settings["david@fail-test.invalid"], "/existing/device-key");
        } else {
            let target = if name == "Redie" {
                "david@key-test.invalid"
            } else if name == "Added" {
                "david@add-test.invalid"
            } else {
                "david@fail-test.invalid"
            };
            let key = PathBuf::from(settings[target].as_str().unwrap());
            assert!(key.is_file());
            assert_eq!(
                fs::metadata(key).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    // Saved-machine API bridges must also forward the enrolled identity after
    // the interactive setup process and any password master have gone away.
    let remote = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["--machine", "Added", "agent", "list"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                root.join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("HERDR_KEY_ROOT", &root)
        .env("HERDR_KEY_REMOTE", root.join("remote"))
        .env("HERDR_KEY_BINARY", env!("CARGO_BIN_EXE_herdr"))
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_SESSION")
        .output()
        .unwrap();
    assert!(
        remote.status.success(),
        "{}",
        String::from_utf8_lossy(&remote.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join("install-count"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    assert_eq!(
        fs::read_to_string(root.join("remote/.ssh/authorized_keys"))
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("ssh-ed25519 "))
            .count(),
        3
    );
}
