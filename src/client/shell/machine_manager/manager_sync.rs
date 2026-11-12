use super::*;
use crate::client::direct_sync::{Action, View};
pub(super) fn rows(view: &View) -> Vec<(String, Hit)> {
    let mut rows = vec![
        (
            if view.listening {
                "Direct sync is available on Tailscale"
            } else {
                "Waiting for Tailscale manager sync"
            }
            .into(),
            Hit::Hint,
        ),
        (
            if view.scanning {
                "Finding running managers…"
            } else {
                "Find / refresh running managers"
            }
            .into(),
            Hit::FindManagers,
        ),
        ("Sync paired managers now".into(), Hit::SyncManagers),
    ];
    if let Some(p) = &view.outgoing {
        if p.accepted {
            rows.push((
                format!("Confirm code {} matches {}…", p.code, p.manager.name),
                Hit::ConfirmPair,
            ));
        }
        rows.push((
            format!(
                "Code {} · {} {} · Cancel pairing",
                p.code,
                if p.accepted {
                    "Approved by"
                } else {
                    "Waiting for"
                },
                p.manager.name
            ),
            Hit::CancelPair,
        ));
    }
    for (i, p) in view.incoming.iter().enumerate() {
        if p.accepted {
            rows.push((
                format!(
                    "Code {} · Waiting for {} to confirm · Cancel",
                    p.code, p.manager.name
                ),
                Hit::DeclinePair(i),
            ));
        } else {
            rows.push((
                format!("Approve code {} from {}…", p.code, p.manager.name),
                Hit::ApprovePair(i),
            ));
            rows.push((format!("Decline {}", p.manager.name), Hit::DeclinePair(i)));
        }
    }
    for (i, p) in view.paired.iter().enumerate() {
        rows.push((
            format!(
                "{} · {} · Forget…",
                p.manager.name,
                if p.error.is_some() {
                    "waiting to reconnect"
                } else if p.last_success.is_some() {
                    "synced"
                } else {
                    "paired"
                }
            ),
            Hit::ForgetManager(i),
        ));
    }
    for (i, m) in view.managers.iter().enumerate() {
        if !view.paired.iter().any(|p| p.manager.id == m.id) {
            rows.push((
                format!("Pair with {} ({})…", m.name, m.ip),
                Hit::PairManager(i),
            ));
        }
    }
    rows.push(("Stop direct manager sync…".into(), Hit::StopManagers));
    rows
}
pub(in crate::client::shell) fn consent(manager: &Manager, hit: Hit) -> Option<(Action, String)> {
    let view = &manager.direct;
    match hit{
        Hit::PairManager(index)=>{let m=view.managers.get(index)?;Some((Action::Pair{manager:m.clone()},format!("Request pairing with {} at {}? Keep both Herdr UIs open. A confirmation code will appear on both computers. Approve on the other computer, then confirm the matching code here. After both approve, server lists merge and sync automatically. This replaces any shared-host sync configuration when pairing completes; SSH keys remain local.",m.name,m.ip)))},
        Hit::ApprovePair(index)=>{let p=view.incoming.get(index)?;Some((Action::Approve{request:p.request.clone(),accepted:true},format!("Share the server list with {} ({})? Compare code {} on both computers. Approve only if you started pairing on that computer. This enables direct sync and replaces any shared-host sync configuration. Server additions, edits and removals will synchronize; SSH credentials remain local.",p.manager.name,p.manager.ip,p.code)))},
        Hit::ConfirmPair=>{let p=view.outgoing.as_ref().filter(|p|p.accepted)?;Some((Action::Confirm{request:p.request.clone()},format!("Confirm code {} matches the code approved on {}? This merges your server lists and enables automatic direct sync while both UIs are open. It replaces any shared-host sync configuration. SSH keys and credentials stay local.",p.code,p.manager.name)))},
        Hit::ForgetManager(index)=>{let p=view.paired.get(index)?;Some((Action::Forget{id:p.manager.id.clone()},format!("Stop sharing with {}? Your server list and remote sessions remain available. This computer will reject further sync from that pairing.",p.manager.name)))},
        Hit::StopManagers=>Some((Action::Disable,"Stop direct manager sync and forget all manager pairings on this computer? The local server list and remote sessions remain available.".into())),
        _=>None,
    }
}
