#![cfg(unix)]

pub mod support;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// Exercise real CLI parsing, protocol guard, and socket routing without PTYs.
fn fake_server(args: &[&str], responses: Vec<Value>) -> (std::process::Output, Vec<Value>) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = std::env::temp_dir().join(format!("hjob-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    let socket = base.join("api.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut received = Vec::new();
        let mut responses = responses.into_iter();
        loop {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "CLI did not send expected requests"
                    );
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("accept: {error}"),
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let response = if request["method"] == "ping" {
                json!({"id":request["id"],"result":{"type":"pong","version":"test","protocol":support::CURRENT_PROTOCOL}})
            } else {
                received.push(request.clone());
                responses.next().expect("unexpected extra operation")
            };
            writeln!(stream, "{response}").unwrap();
            stream.flush().unwrap();
            if responses.len() == 0 {
                return received;
            }
        }
    });
    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(args)
        .env("HERDR_SOCKET_PATH", &socket)
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .output()
        .unwrap();
    let requests = server.join().unwrap();
    std::fs::remove_dir_all(base).unwrap();
    (output, requests)
}

#[test]
fn task_create_preserves_brief_and_criteria() {
    let (output, requests) = fake_server(
        &[
            "task",
            "create",
            "Fix login",
            "--brief",
            "Keep redirects local",
            "--criterion",
            "Login test passes",
            "--branch",
            "fix/login",
        ],
        vec![json!({"id":"create","result":{"type":"task_info","task":{"id":"t1"}}})],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["method"], "task.create");
    assert_eq!(
        requests[0]["params"]["criteria"],
        json!(["Login test passes"])
    );
    assert_eq!(requests[0]["params"]["brief"], "Keep redirects local");
}

#[test]
fn job_wait_polls_only_its_job_until_reported() {
    let (output, requests) = fake_server(
        &["job", "wait", "j9", "--timeout", "3000"],
        vec![
            json!({"id":"wait","result":{"type":"job_info","job":{"id":"j9","state":"running"}}}),
            json!({"id":"wait","result":{"type":"job_info","job":{"id":"j9","state":"succeeded"}}}),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(request["method"], "job.get");
        assert_eq!(request["params"]["job_id"], "j9");
    }
}

#[test]
fn summary_does_not_acknowledge_itself() {
    let (output, requests) = fake_server(
        &["task", "summary", "--consumer", "david"],
        vec![json!({"id":"summary","result":{"type":"task_summary","summary":{"through":15}}})],
    );
    assert!(output.status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["method"], "task.summary");
    assert_eq!(requests[0]["params"]["consumer"], "david");
}

#[test]
fn workflow_step_uses_server_dependency_gate() {
    let (output, requests) = fake_server(
        &["workflow", "step-start", "run1", "test"],
        vec![
            json!({"id":"step","error":{"code":"task_error","message":"dependencies are not accepted"}}),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(requests[0]["method"], "workflow.step.start");
    assert_eq!(
        requests[0]["params"],
        json!({"run_id":"run1","step_id":"test"})
    );
}

#[test]
fn invalid_task_input_fails_before_socket_connection() {
    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["job", "start", "t1", "--target", "coder"])
        .env("HERDR_SOCKET_PATH", "/nonexistent/herdr-job-cli.sock")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("server_not_running"));
}

#[test]
fn workflow_setup_creates_a_checkout_and_launches_its_first_step() {
    let step =
        json!({"id":"implement","title":"Implement","prompt":"Implement the change","criteria":[]});
    let (output, requests) = fake_server(
        &[
            "workflow",
            "setup",
            "saved",
            "--brief",
            "Fix login",
            "--workspace",
            "w1",
            "--branch",
            "fix/login",
            "--kind",
            "codex",
        ],
        vec![
            json!({"result":{"templates":[{"id":"saved","title":"Saved","steps":[step]}]}}),
            json!({"result":{"worktree":{"path":"/worktrees/login"},"workspace":{"id":"w2"},"root_pane":{"pane_id":"w2:p1"}}}),
            json!({"result":{"run":{"id":"run1"},"ready_steps":[step]}}),
            json!({"result":{"agent":{"terminal_id":"terminal1"}}}),
            json!({"result":{"agent":{"terminal_id":"terminal1","name":"wf-run1","agent":"codex","agent_status":"idle","interactive_ready":true}}}),
            json!({"result":{"task":{"id":"t1"}}}),
            json!({"result":{"task":{"id":"t1","cwd":"/worktrees/login"}}}),
            json!({"result":{"job":{"id":"j1","state":"running"}}}),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let methods: Vec<_> = requests
        .iter()
        .map(|request| request["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        vec![
            "workflow.list",
            "worktree.create",
            "workflow.start",
            "agent.start",
            "agent.get",
            "workflow.step.start",
            "task.update",
            "job.start"
        ]
    );
    assert_eq!(requests[2]["params"]["cwd"], "/worktrees/login");
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["resources"]["agent_name"], "wf-run1");
    assert_eq!(response["result"]["resources"]["job"]["id"], "j1");
}
