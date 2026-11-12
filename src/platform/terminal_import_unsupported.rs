use crate::api::schema::{TerminalSessionIdentity, TerminalSessionsReport};
use std::io;

pub(crate) fn discover() -> io::Result<TerminalSessionsReport> {
    Ok(TerminalSessionsReport{supported:false,helper_ready:false,message:"Live terminal import is supported on Linux. This host cannot transfer an existing terminal without restarting its processes.".into(),sessions:vec![]})
}
pub(crate) fn validate(_: &TerminalSessionIdentity) -> io::Result<()> {
    Err(io::Error::other("Live terminal import requires Linux"))
}
pub(crate) fn install_helper() -> io::Result<()> {
    Err(io::Error::other("Live terminal import requires Linux"))
}
pub(crate) fn run(_: &TerminalSessionIdentity, _: bool) -> io::Result<()> {
    Err(io::Error::other("Live terminal import requires Linux"))
}
