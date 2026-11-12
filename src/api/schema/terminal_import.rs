use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalSessionIdentity {
    pub boot_id: String,
    pub uid: u32,
    pub pid: u32,
    pub start_ticks: u64,
    pub session_id: u32,
    pub tty: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExternalTerminalSession {
    pub identity: TerminalSessionIdentity,
    pub terminal: String,
    pub command: String,
    pub cwd: String,
    pub process_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminalSessionsReport {
    pub supported: bool,
    pub helper_ready: bool,
    pub message: String,
    pub sessions: Vec<ExternalTerminalSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalSessionImportParams {
    pub source: TerminalSessionIdentity,
}
