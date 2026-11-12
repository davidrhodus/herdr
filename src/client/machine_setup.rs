//! Client-local setup runtime. Native SSH/installer prompts run in an owned PTY;
//! no setup input, credential, or UI state crosses a terminal endpoint codec.
use portable_pty::{CommandBuilder, PtySize};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

mod discovery;
pub(crate) mod tailscale;
mod terminal_import;
pub(crate) use tailscale::Device as TailscaleDevice;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
pub(crate) fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Debug)]
pub(crate) struct AddServer {
    pub target: String,
    pub username: String,
    pub label: String,
    pub session: String,
    pub port: String,
    pub identity: String,
    pub setup_key: bool,
}
impl AddServer {
    pub fn destination(&self) -> Result<String, String> {
        let address = self.target.trim();
        let username = self.username.trim();
        if username.starts_with('-')
            || username
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "@:/?#[]".contains(c))
        {
            return Err("Enter a username without spaces or address characters".into());
        }
        let (scheme, authority) = address
            .strip_prefix("ssh://")
            .map_or(("", address), |rest| ("ssh://", rest));
        let destination = if username.is_empty() {
            address.to_owned()
        } else if let Some((existing, _)) = authority.rsplit_once('@') {
            if existing != username {
                return Err("The address already has a different username; edit the address or Username field".into());
            }
            address.to_owned()
        } else {
            if authority.is_empty() {
                return Err("Enter a host name or IP address".into());
            }
            format!("{scheme}{username}@{authority}")
        };
        let target = destination.as_str();
        let probe = crate::client::endpoint::SavedSshEndpoint::new("Server", target, "default")?;
        let _ = probe;
        if self.label.trim().len() > crate::client::endpoint::MAX_LABEL_BYTES {
            return Err("Server name is too long".into());
        }
        if !self.session.is_empty() {
            crate::session::validate_name(&self.session)?;
        }
        if self.port.trim().is_empty() {
            return Ok(target.into());
        }
        let port = self
            .port
            .trim()
            .parse::<u16>()
            .map_err(|_| "Port must be between 1 and 65535")?;
        if port == 0 {
            return Err("Port must be between 1 and 65535".into());
        }
        if target.starts_with("ssh://") {
            return Err("Specify the port in either the address or Port field".into());
        }
        let (user, host) = target
            .rsplit_once('@')
            .map_or((String::new(), target), |(user, host)| {
                (format!("{user}@"), host)
            });
        let host = if host.contains(':') && !host.starts_with('[') {
            host.parse::<std::net::Ipv6Addr>()
                .map_err(|_| "Use an IPv6 address, or put an existing port in an ssh:// address")?;
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        Ok(format!("ssh://{user}{host}:{port}"))
    }
}
#[derive(Clone, Debug)]
pub(crate) enum Operation {
    Add(AddServer),
    Reconnect(String),
    Update(String),
    SetupKey(String),
    EnableImports(crate::client::endpoint::SavedSshEndpoint),
    ImportSession {
        profile: crate::client::endpoint::SavedSshEndpoint,
        source: crate::api::schema::TerminalSessionIdentity,
    },
    Rename {
        id: String,
        name: String,
    },
    Enable {
        id: String,
        enabled: bool,
    },
    Remove(String),
    SyncHost,
    SyncUse(String),
    SyncNow,
    SyncLeave,
}
pub(crate) enum Command {
    TerminalScan {
        id: u64,
        request: u64,
        profile: crate::client::endpoint::SavedSshEndpoint,
    },
    TerminalImport {
        id: u64,
        request: u64,
        profile: crate::client::endpoint::SavedSshEndpoint,
        source: crate::api::schema::TerminalSessionIdentity,
    },
    Direct {
        id: u64,
        action: super::direct_sync::Action,
    },
    Tailscale {
        id: u64,
        request: u64,
    },
    Status {
        id: u64,
    },
    Discover {
        id: u64,
    },
    Run {
        id: u64,
        operation: Operation,
        cols: u16,
        rows: u16,
    },
    Input {
        id: u64,
        bytes: Vec<u8>,
    },
    Approval {
        id: u64,
        request: u64,
        accepted: bool,
    },
    Paste {
        id: u64,
        text: String,
    },
    Resize {
        id: u64,
        cols: u16,
        rows: u16,
    },
    Cancel {
        id: u64,
    },
}
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (name, id) = match self {
            Self::TerminalScan { id, .. } => ("TerminalScan", id),
            Self::TerminalImport { id, .. } => ("TerminalImport", id),
            Self::Direct { .. } => return f.write_str("Direct manager action"),
            Self::Tailscale { id, .. } => ("Tailscale", id),
            Self::Status { id } => ("Status", id),
            Self::Discover { id } => ("Discover", id),
            Self::Run { id, .. } => ("Run", id),
            Self::Input { id, .. } => ("Input (redacted)", id),
            Self::Approval { id, .. } => ("Approval", id),
            Self::Paste { id, .. } => ("Paste (redacted)", id),
            Self::Resize { id, .. } => ("Resize", id),
            Self::Cancel { id } => ("Cancel", id),
        };
        f.debug_struct(name).field("id", id).finish()
    }
}
#[derive(Debug)]
pub(crate) enum Event {
    TerminalScan {
        id: u64,
        request: u64,
        profile: String,
        result: Result<crate::api::schema::TerminalSessionsReport, String>,
    },
    TerminalImported {
        id: u64,
        request: u64,
        profile: String,
        result: Result<String, String>,
    },
    DirectError {
        id: u64,
        error: String,
    },
    Tailscale {
        id: u64,
        request: u64,
        result: Result<Vec<TailscaleDevice>, String>,
    },
    Approval {
        id: u64,
        prompt: crate::remote::setup_progress::Approval,
    },
    Phase {
        id: u64,
        phase: crate::remote::setup_progress::Phase,
    },
    Status {
        id: u64,
        text: String,
    },
    Suggestions {
        id: u64,
        targets: Vec<String>,
        sync: String,
    },
    Screen {
        id: u64,
        buffer: Buffer,
        cursor: Option<(u16, u16)>,
    },
    Finished {
        id: u64,
        success: bool,
        message: String,
    },
}
#[derive(Default)]
pub(super) struct Runtime {
    worker: Option<Worker>,
}
struct Worker {
    tx: Option<mpsc::Sender<Command>>,
    rx: Option<mpsc::Receiver<Event>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Runtime {
    pub fn send(&mut self, command: Command) {
        let worker = self.worker.get_or_insert_with(|| {
            let (tx, commands) = mpsc::channel();
            let (events, rx) = mpsc::sync_channel(8);
            let thread = std::thread::spawn(move || run(commands, events));
            Worker {
                tx: Some(tx),
                rx: Some(rx),
                thread: Some(thread),
            }
        });
        if let Some(tx) = &worker.tx {
            let _ = tx.send(command);
        }
    }
    pub fn poll(&self) -> Option<Event> {
        self.worker.as_ref()?.rx.as_ref()?.try_recv().ok()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.tx.take();
        self.rx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.worker.take();
    }
}
fn run(commands: mpsc::Receiver<Command>, events: mpsc::SyncSender<Event>) {
    let mut pending = None;
    loop {
        let command = match pending.take().or_else(|| commands.recv().ok()) {
            Some(c) => c,
            None => return,
        };
        match command {
            Command::TerminalScan {
                id,
                request,
                profile,
            } => {
                let events = events.clone();
                std::thread::spawn(move || {
                    let result = terminal_import::discover(&profile);
                    let profile = profile.id.to_string();
                    let _ = events.send(Event::TerminalScan {
                        id,
                        request,
                        profile,
                        result,
                    });
                });
            }
            Command::TerminalImport {
                id,
                request,
                profile,
                source,
            } => {
                let events = events.clone();
                std::thread::spawn(move || {
                    let result = terminal_import::import(&profile, &source);
                    let profile = profile.id.to_string();
                    let _ = events.send(Event::TerminalImported {
                        id,
                        request,
                        profile,
                        result,
                    });
                });
            }
            Command::Direct { id, action } => {
                if let Err(error) = super::direct_sync::submit(action) {
                    let _ = events.send(Event::DirectError { id, error });
                }
            }
            Command::Tailscale { id, request } => {
                let events = events.clone();
                std::thread::spawn(move || {
                    let result = tailscale::devices();
                    let _ = events.send(Event::Tailscale {
                        id,
                        request,
                        result,
                    });
                });
            }
            Command::Status { id } => {
                let _ = events.send(Event::Status {
                    id,
                    text: sync_status(),
                });
            }
            Command::Discover { id } => {
                let events = events.clone();
                std::thread::spawn(move || {
                    let targets = discovery::targets();
                    let sync = sync_status();
                    let _ = events.send(Event::Suggestions { id, targets, sync });
                });
            }
            Command::Run {
                id,
                operation,
                cols,
                rows,
            } => match session(&commands, &events, id, operation, cols, rows) {
                Ok(next) => pending = next,
                Err(error) => {
                    let _ = events.send(Event::Finished {
                        id,
                        success: false,
                        message: error,
                    });
                }
            },
            _ => {}
        }
    }
}
fn builder(operation: &Operation) -> Result<CommandBuilder, String> {
    let mut command = CommandBuilder::new(std::env::current_exe().map_err(|e| e.to_string())?);
    match operation {
        Operation::Add(form) => {
            let target = form.destination()?;
            if !form.identity.trim().is_empty() {
                let path = crate::remote::device_identity::absolute(form.identity.trim())?;
                command.env(crate::remote::device_identity::SETUP_TARGET, &target);
                command.env(crate::remote::device_identity::SETUP_PATH, path);
            }
            command.args(["machine", "add", &target]);
            if form.setup_key {
                command.arg("--setup-key");
            }
            if !form.label.trim().is_empty() {
                command.args(["--label", form.label.trim()]);
            }
            if !form.session.trim().is_empty() {
                command.args(["--remote-session", form.session.trim()]);
            }
        }
        Operation::Reconnect(id) => {
            command.args(["machine", "reconnect", id]);
        }
        Operation::Update(id) => {
            command.args(["machine", "update", id]);
        }
        Operation::SetupKey(id) => {
            command.args(["machine", "setup-key", id]);
        }
        Operation::EnableImports(profile) => {
            command.args([
                "machine",
                "enable-imports",
                profile.id.as_str(),
                "--expected-target",
                &profile.target,
                "--expected-session",
                &profile.session,
            ]);
        }
        Operation::ImportSession { profile, source } => {
            command.args([
                "--machine",
                profile.id.as_str(),
                "terminal-import",
                "start",
                &crate::terminal_import::encode(source).map_err(|e| e.to_string())?,
            ]);
        }
        Operation::Rename { id, name } => {
            command.args(["machine", "rename", id, "--label", name]);
        }
        Operation::Enable { id, enabled } => {
            command.args(["machine", if *enabled { "enable" } else { "disable" }, id]);
        }
        Operation::Remove(id) => {
            command.args(["machine", "remove", id]);
        }
        Operation::SyncHost => {
            command.args(["sync", "host"]);
        }
        Operation::SyncUse(target) => {
            command.args(["sync", "use", target]);
        }
        Operation::SyncNow => {
            command.args(["sync", "now"]);
        }
        Operation::SyncLeave => {
            command.args(["sync", "leave"]);
        }
    }
    command.env("TERM", "xterm-256color");
    command.env("SSH_ASKPASS_REQUIRE", "never");
    command.env_remove("SSH_ASKPASS");
    Ok(command)
}
struct SetupFiles(std::path::PathBuf);
impl Drop for SetupFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn sync_status() -> String {
    match crate::client::endpoint::catalog_sync::status() {
        Ok(value) if value["enabled"] == false => "Server-list sync is off".into(),
        Ok(value) if value["source"]["kind"] == "direct" => {
            "Direct manager sync is enabled over Tailscale".into()
        }
        Ok(value) if value["source"]["kind"] == "local" => {
            "Shared list is hosted on this computer".into()
        }
        Ok(value) => {
            let target = value["source"]["target"].as_str().unwrap_or("peer");
            let success = value["last_success"].as_u64().map_or_else(
                || "Not synced yet".into(),
                |at| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let age = now.saturating_sub(at);
                    if age < 60 {
                        "Last synced just now".into()
                    } else {
                        format!("Last synced {} minutes ago", age / 60)
                    }
                },
            );
            let error = value["last_error"]
                .as_str()
                .map(|e| format!("\nConnection error: {}", e.replace(['\n', '\r'], " ")))
                .unwrap_or_default();
            format!("Shared list from {target}\n{success}{error}")
        }
        Err(_) => "Shared-list status is unavailable".into(),
    }
}
struct OwnedChild {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reaped: bool,
    _group: crate::platform::SetupProcessGroup,
}
impl std::ops::Deref for OwnedChild {
    type Target = Box<dyn portable_pty::Child + Send + Sync>;
    fn deref(&self) -> &Self::Target {
        &self.child
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            crate::platform::cancel_setup_process(self.child.process_id());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn session(
    commands: &mpsc::Receiver<Command>,
    events: &mpsc::SyncSender<Event>,
    id: u64,
    operation: Operation,
    cols: u16,
    rows: u16,
) -> Result<Option<Command>, String> {
    tracing::info!(request_id = id, "starting interactive server setup");
    session_with_command(commands, events, id, builder(&operation)?, cols, rows)
}
fn session_with_command(
    commands: &mpsc::Receiver<Command>,
    events: &mpsc::SyncSender<Event>,
    id: u64,
    mut launch: CommandBuilder,
    cols: u16,
    rows: u16,
) -> Result<Option<Command>, String> {
    let cols = cols.clamp(1, 160);
    let rows = rows.clamp(1, 60);
    let pair = portable_pty::native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;
    let files = SetupFiles(
        crate::platform::create_remote_ssh_config_dir("machine-setup")
            .map_err(|e| e.to_string())?,
    );
    let progress_path = files.0.join("progress");
    drop(crate::platform::create_private_state_file(&progress_path).map_err(|e| e.to_string())?);
    let answer_path = progress_path.with_extension("answer");
    drop(crate::platform::create_private_state_file(&answer_path).map_err(|e| e.to_string())?);
    launch.env("HERDR_SETUP_PARENT_PID", std::process::id().to_string());
    let mut progress = std::fs::File::open(&progress_path).map_err(|e| e.to_string())?;
    launch.env(crate::remote::setup_progress::ENV, &progress_path);
    let child = pair
        .slave
        .spawn_command(launch)
        .map_err(|e| e.to_string())?;
    let group = crate::platform::SetupProcessGroup::new(child.process_id());
    let mut child = OwnedChild {
        child,
        _group: group,
        reaped: false,
    };
    drop(pair.slave);
    let pid = child.process_id();
    let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let (input, writes) = mpsc::sync_channel::<Vec<u8>>(32);
    std::thread::spawn(move || {
        for bytes in writes {
            if writer
                .write_all(&bytes)
                .and_then(|()| writer.flush())
                .is_err()
            {
                break;
            }
        }
    });
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let (output, bytes) = mpsc::sync_channel(8);
    std::thread::spawn(move || {
        let mut block = [0u8; 8192];
        loop {
            match reader.read(&mut block) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if output.send(block[..n].to_vec()).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let mut terminal = ghostty_vt::Terminal::new(cols, rows, 0).map_err(|e| e.to_string())?;
    let replies = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let sink = replies.clone();
    terminal
        .set_write_pty_callback(move |data| {
            if let Ok(mut bytes) = sink.lock() {
                bytes.extend_from_slice(data);
            }
        })
        .map_err(|e| e.to_string())?;
    let mut render = ghostty_vt::RenderState::new().map_err(|e| e.to_string())?;
    let mut dirty = true;
    let mut last = std::time::Instant::now() - Duration::from_secs(1);
    let mut progress_buffer = Vec::new();
    loop {
        let mut updates = Vec::new();
        if std::io::Read::by_ref(&mut progress)
            .take(16384)
            .read_to_end(&mut updates)
            .is_ok()
        {
            progress_buffer.extend_from_slice(&updates);
        }
        if progress_buffer.len() > 65536 {
            return Err("Invalid setup progress message".into());
        }
        while let Some(end) = progress_buffer.iter().position(|byte| *byte == b'\n') {
            let line = String::from_utf8_lossy(&progress_buffer[..end]).into_owned();
            progress_buffer.drain(..=end);
            if let Some(phase) = crate::remote::setup_progress::Phase::parse(&line) {
                let _ = events.try_send(Event::Phase { id, phase });
            } else if let Ok(prompt) =
                serde_json::from_str::<crate::remote::setup_progress::Approval>(&line)
            {
                let _ = events.send(Event::Approval { id, prompt });
            }
        }
        match commands.recv_timeout(Duration::from_millis(20)) {
            Ok(Command::Approval {
                id: incoming,
                request,
                accepted,
            }) if incoming == id => {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&answer_path)
                    .map_err(|e| e.to_string())?;
                serde_json::to_writer(
                    &mut file,
                    &crate::remote::setup_progress::Answer { request, accepted },
                )
                .map_err(|e| e.to_string())?;
                file.flush().map_err(|e| e.to_string())?;
            }
            Ok(Command::Paste { id: incoming, text }) if incoming == id => {
                if text.len() > 64 * 1024 {
                    return Err("Paste is too large for the setup terminal".into());
                }
                let bytes = if terminal.mode_get(2004).unwrap_or(false) {
                    format!("\x1b[200~{text}\x1b[201~").into_bytes()
                } else {
                    text.into_bytes()
                };
                input
                    .try_send(bytes)
                    .map_err(|_| "Input is busy; wait for the current prompt".to_string())?;
            }
            Ok(Command::Input {
                id: incoming,
                bytes,
            }) if incoming == id => {
                input
                    .try_send(bytes)
                    .map_err(|_| "Input is busy; wait for the current prompt".to_string())?;
            }
            Ok(Command::Resize {
                id: incoming,
                cols,
                rows,
            }) if incoming == id => {
                let cols = cols.clamp(1, 160);
                let rows = rows.clamp(1, 60);
                pair.master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .map_err(|e| e.to_string())?;
                terminal
                    .resize(cols, rows, 8, 16)
                    .map_err(|e| e.to_string())?;
                dirty = true;
            }
            Ok(Command::Cancel { id: incoming }) if incoming == id => {
                crate::platform::cancel_setup_process(pid);
                let _ = child.kill();
                let _ = child.wait();
                child.reaped = true;
                return Ok(None);
            }
            Ok(next @ Command::Run { .. }) => {
                crate::platform::cancel_setup_process(pid);
                let _ = child.kill();
                let _ = child.wait();
                child.reaped = true;
                return Ok(Some(next));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                crate::platform::cancel_setup_process(pid);
                let _ = child.kill();
                let _ = child.wait();
                child.reaped = true;
                return Ok(None);
            }
            _ => {}
        }
        for _ in 0..32 {
            match bytes.try_recv() {
                Ok(data) => {
                    terminal.write(&data);
                    dirty = true;
                }
                Err(_) => break,
            }
        }
        if let Ok(mut reply) = replies.lock() {
            if !reply.is_empty() {
                input
                    .try_send(std::mem::take(&mut *reply))
                    .map_err(|_| "Setup input is unavailable".to_string())?;
            }
        }
        if dirty && last.elapsed() >= Duration::from_millis(50) {
            let (buffer, cursor) = snapshot(&terminal, &mut render)?;
            if events
                .try_send(Event::Screen { id, buffer, cursor })
                .is_ok()
            {
                dirty = false;
                last = std::time::Instant::now();
            }
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            tracing::info!(
                request_id = id,
                success = status.success(),
                "interactive server setup finished"
            );
            child.reaped = true;
            let deadline = std::time::Instant::now() + Duration::from_millis(250);
            while std::time::Instant::now() < deadline {
                match bytes.recv_timeout(Duration::from_millis(20)) {
                    Ok(data) => terminal.write(&data),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            let (buffer, cursor) = snapshot(&terminal, &mut render)?;
            let _ = events.send(Event::Screen { id, buffer, cursor });
            let _ = events.send(Event::Finished {
                id,
                success: status.success(),
                message: if status.success() {
                    "Ready. Changes to the server list are saved.".into()
                } else {
                    "Setup did not finish. Review the message above, then retry.".into()
                },
            });
            let _ = events.send(Event::Status {
                id,
                text: sync_status(),
            });
            return Ok(None);
        }
    }
}
fn snapshot(
    terminal: &ghostty_vt::Terminal,
    render: &mut ghostty_vt::RenderState,
) -> Result<(Buffer, Option<(u16, u16)>), String> {
    render.update(terminal).map_err(|e| e.to_string())?;
    let cols = render.cols().map_err(|e| e.to_string())?;
    let height = render.rows().map_err(|e| e.to_string())?;
    let mut buffer = Buffer::empty(Rect::new(0, 0, cols, height));
    let mut rows = ghostty_vt::RowIterator::new().map_err(|e| e.to_string())?;
    let mut cells = ghostty_vt::RowCells::new().map_err(|e| e.to_string())?;
    let mut rows = render
        .populate_row_iterator(&mut rows)
        .map_err(|e| e.to_string())?;
    let mut y = 0;
    while rows.next() {
        let mut row = rows.populate_cells(&mut cells).map_err(|e| e.to_string())?;
        let mut x = 0;
        while row.next() {
            let basic = row.basic_data().map_err(|e| e.to_string())?;
            let mut style = Style::default();
            if let Some(c) = row.fg_color().map_err(|e| e.to_string())? {
                style = style.fg(Color::Rgb(c.r, c.g, c.b));
            }
            if let Some(c) = row.bg_color().map_err(|e| e.to_string())? {
                style = style.bg(Color::Rgb(c.r, c.g, c.b));
            }
            if basic.style.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if basic.style.inverse {
                style = style.add_modifier(Modifier::REVERSED);
            }
            buffer[(x, y)].skip = basic.wide == ghostty_vt::CellWide::SpacerTail;
            buffer[(x, y)]
                .set_symbol(&row.grapheme_text().map_err(|e| e.to_string())?)
                .set_style(style);
            x += 1;
        }
        y += 1;
    }
    let cursor = render.cursor().map_err(|e| e.to_string())?;
    let cursor = cursor
        .viewport
        .filter(|_| cursor.visible)
        .map(|v| (v.x, v.y));
    render.clean().map_err(|e| e.to_string())?;
    Ok((buffer, cursor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::IsTerminal;
    fn form(target: &str, port: &str) -> AddServer {
        AddServer {
            target: target.into(),
            username: String::new(),
            label: String::new(),
            session: String::new(),
            port: port.into(),
            identity: String::new(),
            setup_key: true,
        }
    }
    #[test]
    fn explicit_ports_support_ipv6_and_reject_ambiguous_addresses() {
        assert_eq!(
            form("you@2001:db8::1", "2222").destination().unwrap(),
            "ssh://you@[2001:db8::1]:2222"
        );
        assert_eq!(form("host", "22").destination().unwrap(), "ssh://host:22");
        assert!(form("host", "0").destination().is_err());
        assert!(form("ssh://host:22", "23").destination().is_err());
        assert!(form("ssh://you:secret@host", "").destination().is_err());
    }
    #[test]
    fn separate_username_composes_hosts_aliases_uris_and_ipv6_without_conflicts() {
        for (host, port, expected) in [
            ("192.168.1.50", "", "david@192.168.1.50"),
            ("workbox", "", "david@workbox"),
            ("ssh://host:2222", "", "ssh://david@host:2222"),
            ("2001:db8::1", "2222", "ssh://david@[2001:db8::1]:2222"),
            ("david@host", "", "david@host"),
        ] {
            let mut value = form(host, port);
            value.username = " david ".into();
            assert_eq!(value.destination().unwrap(), expected);
        }
        for (host, username) in [
            ("you@host", "david"),
            ("ssh://you@host", "david"),
            ("host", "bad user"),
            ("host", "user:password"),
            ("host", "other@host"),
            ("host", "-option"),
            ("", "david"),
        ] {
            let mut value = form(host, "");
            value.username = username.into();
            assert!(value.destination().is_err(), "{host} / {username}");
        }
        assert_eq!(form("workbox", "").destination().unwrap(), "workbox");
    }
    #[test]
    fn credential_inputs_are_redacted_and_native_ssh_never_uses_askpass() {
        for command in [
            Command::Input {
                id: 1,
                bytes: b"test-secret".to_vec(),
            },
            Command::Paste {
                id: 1,
                text: "test-secret".into(),
            },
        ] {
            assert!(!format!("{command:?}").contains("test-secret"));
        }
        let command = builder(&Operation::Add(form("host", ""))).unwrap();
        assert_eq!(
            command.get_env("SSH_ASKPASS_REQUIRE"),
            Some(std::ffi::OsStr::new("never"))
        );
        assert_eq!(command.get_env("SSH_ASKPASS"), None);
    }
    #[test]
    fn setup_identity_is_scoped_and_does_not_change_existing_local_settings() {
        let root = crate::platform::create_remote_ssh_config_dir("identity-test").unwrap();
        let path = root.join("key");
        std::fs::write(&path, b"fixture only").unwrap();
        let target = format!("test-{}", next_id());
        let before = crate::remote::device_identity::get(&target);
        let mut input = form(&target, "");
        input.identity = path.to_string_lossy().into_owned();
        let command = builder(&Operation::Add(input)).unwrap();
        assert_eq!(
            command.get_env(crate::remote::device_identity::SETUP_TARGET),
            Some(std::ffi::OsStr::new(&target))
        );
        assert!(std::path::Path::new(
            command
                .get_env(crate::remote::device_identity::SETUP_PATH)
                .unwrap()
        )
        .is_absolute());
        assert_eq!(crate::remote::device_identity::get(&target), before);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "owned PTY test subprocess"]
    fn native_setup_fixture() {
        match std::env::var("HERDR_TEST_SETUP_FIXTURE").unwrap().as_str() {
            "interactive" => {
                assert!(std::io::stdin().is_terminal());
                assert!(std::io::stderr().is_terminal());
                crossterm::terminal::enable_raw_mode().unwrap();
                print!("Password:");
                std::io::stdout().flush().unwrap();
                let mut secret = [0u8; 4];
                std::io::stdin().read_exact(&mut secret).unwrap();
                assert_eq!(&secret, b"abcd");
                println!("\r\nAuthenticated");
                crossterm::terminal::disable_raw_mode().unwrap();
            }
            "immediate" => {
                eprintln!("Immediate final diagnostic");
            }
            "approval" | "decline" => {
                assert_eq!(
                    crate::remote::setup_progress::confirm(
                        "Install Herdr",
                        "Install the test package?".into(),
                        false
                    )
                    .unwrap(),
                    Some(std::env::var("HERDR_TEST_SETUP_FIXTURE").unwrap() == "approval")
                );
                println!("Answered through private channel");
            }
            "hang" => {
                println!("Waiting for approval");
                std::io::stdout().flush().unwrap();
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            _ => panic!("invalid fixture"),
        }
    }
    fn fixture(mode: &str) -> CommandBuilder {
        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "client::machine_setup::tests::native_setup_fixture",
            "--ignored",
            "--nocapture",
        ]);
        command.env("HERDR_TEST_SETUP_FIXTURE", mode);
        command
    }
    fn contents(buffer: &Buffer) -> String {
        buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .concat()
    }
    #[test]
    fn native_pty_prompts_capture_input_without_echoing_credentials_or_losing_exit_output() {
        for mode in ["interactive", "immediate"] {
            let (tx, rx) = mpsc::channel();
            let (events, received) = mpsc::sync_channel(32);
            let command = fixture(mode);
            let worker =
                std::thread::spawn(move || session_with_command(&rx, &events, 42, command, 80, 16));
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            let mut replied = false;
            let mut transcript = String::new();
            let mut finished = false;
            while std::time::Instant::now() < deadline {
                match received.recv_timeout(Duration::from_millis(100)) {
                    Ok(Event::Screen { buffer, .. }) => {
                        let text = contents(&buffer);
                        assert!(!text.contains("abcd"));
                        if mode == "interactive" && !replied && text.contains("Password:") {
                            tx.send(Command::Input {
                                id: 42,
                                bytes: b"abcd".to_vec(),
                            })
                            .unwrap();
                            replied = true;
                        }
                        transcript.push_str(&text);
                    }
                    Ok(Event::Finished { success, .. }) => {
                        assert!(success);
                        finished = true;
                        break;
                    }
                    _ => {}
                }
            }
            if !finished {
                let _ = tx.send(Command::Cancel { id: 42 });
            }
            worker.join().unwrap().unwrap();
            assert!(finished, "{transcript}");
            assert!(transcript.contains(if mode == "interactive" {
                "Authenticated"
            } else {
                "Immediate final diagnostic"
            }));
        }
    }
    #[test]
    fn native_pty_cancellation_reaps_the_owned_helper() {
        let (tx, rx) = mpsc::channel();
        let (events, received) = mpsc::sync_channel(32);
        let worker = std::thread::spawn(move || {
            session_with_command(&rx, &events, 7, fixture("hang"), 80, 16)
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut waiting = false;
        while std::time::Instant::now() < deadline {
            if let Ok(Event::Screen { buffer, .. }) =
                received.recv_timeout(Duration::from_millis(100))
            {
                if contents(&buffer).contains("Waiting for approval") {
                    waiting = true;
                    break;
                }
            }
        }
        tx.send(Command::Cancel { id: 7 }).unwrap();
        assert!(worker.join().unwrap().unwrap().is_none());
        assert!(waiting);
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;
    #[test]
    fn native_approval_requires_matching_private_answer_not_terminal_input() {
        for accepted in [false, true] {
            let (tx, rx) = mpsc::channel();
            let (events, received) = mpsc::sync_channel(32);
            let mut launch = CommandBuilder::new(std::env::current_exe().unwrap());
            launch.args([
                "--exact",
                "client::machine_setup::tests::native_setup_fixture",
                "--ignored",
                "--nocapture",
            ]);
            launch.env(
                "HERDR_TEST_SETUP_FIXTURE",
                if accepted { "approval" } else { "decline" },
            );
            let worker =
                std::thread::spawn(move || session_with_command(&rx, &events, 75, launch, 80, 16));
            tx.send(Command::Input {
                id: 75,
                bytes: b"yes\n".to_vec(),
            })
            .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            let mut prompt = None;
            while std::time::Instant::now() < deadline {
                match received.recv_timeout(Duration::from_millis(100)) {
                    Ok(Event::Approval { id, prompt: p }) => {
                        assert_eq!(id, 75);
                        prompt = Some(p);
                        break;
                    }
                    Ok(Event::Finished { .. }) => panic!("TTY input approved setup"),
                    _ => {}
                }
            }
            let p = prompt.expect("approval event");
            assert_eq!(p.title, "Install Herdr");
            tx.send(Command::Approval {
                id: 75,
                request: p.request + 1,
                accepted: true,
            })
            .unwrap();
            let wait = std::time::Instant::now() + Duration::from_millis(150);
            while std::time::Instant::now() < wait {
                assert!(
                    !matches!(
                        received.recv_timeout(Duration::from_millis(20)),
                        Ok(Event::Finished { .. })
                    ),
                    "stale answer approved setup"
                );
            }
            tx.send(Command::Approval {
                id: 75,
                request: p.request,
                accepted,
            })
            .unwrap();
            let mut finished = false;
            while std::time::Instant::now() < deadline {
                if let Ok(Event::Finished { success, .. }) =
                    received.recv_timeout(Duration::from_millis(100))
                {
                    assert!(success);
                    finished = true;
                    break;
                }
            }
            if !finished {
                let _ = tx.send(Command::Cancel { id: 75 });
            }
            worker.join().unwrap().unwrap();
            assert!(finished);
        }
    }
}
