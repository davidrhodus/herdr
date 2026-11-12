//! Real headless-server persistence and task API smoke tests, isolated from user sessions.
#![cfg(unix)]
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server {
    child: Child,
    socket: PathBuf,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Server {
    fn start(base: &Path) -> Self {
        let runtime = base.join("run");
        std::fs::create_dir_all(&runtime).unwrap();
        let socket = runtime.join("api.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_herdr"))
            .arg("server")
            .env("XDG_CONFIG_HOME", base.join("config"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("HERDR_SOCKET_PATH", &socket)
            .env("HERDR_CLIENT_SOCKET_PATH", runtime.join("client.sock"))
            .env("SHELL", "/bin/sh")
            .env_remove("HERDR_ENV")
            .env_remove("HERDR_SESSION")
            .env_remove("HERDR_STARTUP_CWD")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut server = Self { child, socket };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if UnixStream::connect(&server.socket).is_ok() {
                return server;
            }
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "test server exited before opening socket"
            );
            assert!(Instant::now() < deadline, "test server socket timeout");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    fn call(&self, method: &str, params: Value) -> Value {
        let mut stream = UnixStream::connect(&self.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(
            stream,
            "{}",
            json!({"id":"runtime-test","method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

#[test]
fn real_server_tasks_workflows_and_acknowledgements_survive_restart() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = Path::new("/tmp").join(format!("h-task-{}-{nonce}", std::process::id()));
    let server = Server::start(&base);
    let created = server.call(
        "task.create",
        json!({"title":"Fix redirects","brief":"Preserve destination","criteria":["Login passes"]}),
    );
    assert!(created.get("error").is_none(), "{created}");
    let task_id = created["result"]["task"]["id"].clone();
    let run = server.call(
        "workflow.start",
        json!({"template_id":"implement-test-review","brief":"Fix login"}),
    );
    assert_eq!(run["result"]["ready_steps"][0]["id"], "implement");
    let run_id = run["result"]["run"]["id"].clone();
    let premature = server.call(
        "workflow.step.start",
        json!({"run_id":run_id,"step_id":"test"}),
    );
    assert!(premature.get("error").is_some());
    let summary = server.call("task.summary", json!({"consumer":"david"}));
    let through = summary["result"]["summary"]["through"].clone();
    let ack = server.call(
        "task.acknowledge",
        json!({"consumer":"david","through":through}),
    );
    assert_eq!(ack["result"]["type"], "ok");
    let rejected = server.call(
        "job.start",
        json!({"task_id":task_id,"request_key":"missing","target":"missing-agent"}),
    );
    assert!(rejected.get("error").is_some());
    drop(server);
    // The API and client sockets belong to the test server killed above.
    let _ = std::fs::remove_file(base.join("run/api.sock"));
    let _ = std::fs::remove_file(base.join("run/client.sock"));
    let server = Server::start(&base);
    let recovered = server.call("task.get", json!({"task_id":task_id}));
    assert_eq!(recovered["result"]["task"]["title"], "Fix redirects");
    let summary = server.call("task.summary", json!({"consumer":"david"}));
    assert_eq!(summary["result"]["summary"]["after"], through);
    assert!(summary["result"]["summary"]["activities"]
        .as_array()
        .unwrap()
        .is_empty());
    let workflow = server.call("workflow.get", json!({"run_id":run_id}));
    assert_eq!(workflow["result"]["ready_steps"][0]["id"], "implement");
    let activity = server.call("task.activity", json!({"query":"destination"}));
    assert_eq!(
        activity["result"]["activities"].as_array().unwrap().len(),
        1
    );
    drop(server);
    std::fs::remove_dir_all(base).unwrap();
}
