use crate::{
    api::{
        client::{ApiClient, ConnectionTarget},
        schema::{
            EmptyParams, Method, Request, TerminalSessionIdentity, TerminalSessionImportParams,
            TerminalSessionsReport,
        },
    },
    client::endpoint::SavedSshEndpoint,
};
use std::time::Duration;

fn request(profile: &SavedSshEndpoint, method: Method) -> Result<serde_json::Value, String> {
    // Capture the full selected server, rather than resolving its ID again in
    // a child CLI after the shared catalog may have been edited.
    let bridge = crate::remote::SavedSshApiBridge::start(
        profile.id.as_str(),
        &profile.target,
        &profile.session,
        true,
        true,
    )
    .map_err(|e| e.to_string())?;
    let client = ApiClient::for_target(ConnectionTarget::SocketPath(
        bridge.socket_path().to_owned(),
    ));
    let response = client
        .request_value_with_timeout(
            &Request {
                id: "terminal-import-client".into(),
                method,
            },
            Duration::from_secs(30),
        )
        .map_err(|e| e.to_string())?;
    if let Some(error) = response.get("error") {
        if error["code"] == "invalid_request" {
            return Err("Update this server's Herdr build before using terminal import.".into());
        }
        return Err(error["message"]
            .as_str()
            .unwrap_or("Terminal import failed")
            .chars()
            .take(3000)
            .collect());
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| "Invalid server response".into())
}
pub(super) fn discover(profile: &SavedSshEndpoint) -> Result<TerminalSessionsReport, String> {
    let result = request(
        profile,
        Method::TerminalSessionsList(EmptyParams::default()),
    )?;
    serde_json::from_value(result["report"].clone()).map_err(|e| e.to_string())
}
pub(super) fn import(
    profile: &SavedSshEndpoint,
    source: &TerminalSessionIdentity,
) -> Result<String, String> {
    let result = request(
        profile,
        Method::TerminalSessionImport(TerminalSessionImportParams {
            source: source.clone(),
        }),
    )?;
    result["workspace"]["workspace_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "Server did not return the import workspace".into())
}
