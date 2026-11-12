//! Optional, private progress channel for the embedded setup UI. Never carries
//! authentication input, command output, identities, or other secret material.
use std::io::Write;
pub(crate) const ENV: &str = "HERDR_SETUP_PROGRESS_FILE";
#[derive(Clone, Copy, Debug)]
pub(crate) enum Phase {
    Connecting,
    Checking,
    Sessions,
    Choosing,
    Approval,
    Downloading,
    Installing,
    Starting,
}
impl Phase {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting with SSH",
            Self::Checking => "Checking Herdr installation",
            Self::Sessions => "Finding running sessions",
            Self::Choosing => "Choose a remote session below",
            Self::Approval => "Review the installation or update request below",
            Self::Downloading => "Preparing the remote package",
            Self::Installing => "Installing Herdr on the server",
            Self::Starting => "Starting the remote server",
        }
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "Connecting" => Self::Connecting,
            "Checking" => Self::Checking,
            "Sessions" => Self::Sessions,
            "Choosing" => Self::Choosing,
            "Approval" => Self::Approval,
            "Downloading" => Self::Downloading,
            "Installing" => Self::Installing,
            "Starting" => Self::Starting,
            _ => return None,
        })
    }
}
pub(crate) fn emit(phase: Phase) {
    if let Some(path) = std::env::var_os(ENV) {
        if let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) {
            let _ = writeln!(file, "{phase:?}");
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Approval {
    pub request: u64,
    pub title: String,
    pub detail: String,
    pub restart: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Answer {
    pub request: u64,
    pub accepted: bool,
}
/// GUI consent travels through its private channel, never through buffered SSH
/// password/session input. Ordinary CLI callers keep their existing TTY prompt.
pub(crate) fn confirm(title: &str, detail: String, restart: bool) -> std::io::Result<Option<bool>> {
    use std::io::Read;
    static REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let Some(path) = std::env::var_os(ENV).map(std::path::PathBuf::from) else {
        return Ok(None);
    };
    let prompt = Approval {
        request: REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        title: title.into(),
        detail,
        restart,
    };
    let mut payload = serde_json::to_vec(&prompt).map_err(std::io::Error::other)?;
    payload.push(b'\n');
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(&payload)?;
    let answer = path.with_extension("answer");
    loop {
        if let Ok(file) = std::fs::File::open(&answer) {
            let mut bytes = Vec::new();
            file.take(1024).read_to_end(&mut bytes)?;
            if let Ok(response) = serde_json::from_slice::<Answer>(&bytes) {
                if response.request == prompt.request {
                    emit(Phase::Checking);
                    return Ok(Some(response.accepted));
                }
            }
        }
        if let Some(pid) = std::env::var("HERDR_SETUP_PARENT_PID")
            .ok()
            .and_then(|p| p.parse::<u32>().ok())
        {
            if !crate::platform::process_exists(pid) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "Setup UI disconnected",
                ));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
