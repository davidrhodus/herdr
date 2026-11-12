//! Client-owned, explicitly paired replication over authenticated Tailscale TCP.
//! This transport and its messages are separate from every terminal endpoint codec.
mod service;
mod store;
#[cfg(test)]
mod tests;
mod wire;
use super::endpoint::catalog_sync;
use super::machine_setup::tailscale::{Network, Node};
use serde::{Deserialize, Serialize};
use std::{net::IpAddr, path::Path};
use store::{now, random};

pub(crate) const PORT: u16 = 45971;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manager {
    pub id: String,
    pub node: String,
    pub name: String,
    pub ip: IpAddr,
}
impl Manager {
    fn validate(&self) -> Result<(), String> {
        if !herdr_sync::valid_id(&self.id)
            || self.node.is_empty()
            || self.node.len() > 128
            || self.name.is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
            || !super::machine_setup::tailscale::tailscale_ip(self.ip)
        {
            Err("Invalid manager identity".into())
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PairRequest {
    pub request: String,
    pub manager: Manager,
    pub code: String,
    pub expires: u64,
    pub accepted: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Paired {
    pub manager: Manager,
    pub last_success: Option<u64>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct View {
    pub managers: Vec<Manager>,
    pub paired: Vec<Paired>,
    pub incoming: Vec<PairRequest>,
    pub outgoing: Option<PairRequest>,
    pub scanning: bool,
    pub error: Option<String>,
    pub listening: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub(crate) enum Action {
    Discover,
    Pair { manager: Manager },
    Approve { request: String, accepted: bool },
    Confirm { request: String },
    Cancel { request: String },
    Forget { id: String },
    SyncNow,
    Disable,
}

pub(crate) fn submit(action: Action) -> Result<(), String> {
    submit_at(&store::root(), action)
}
fn submit_at(root: &Path, action: Action) -> Result<(), String> {
    let queue = root.join("direct-sync-commands");
    std::fs::create_dir_all(&queue).map_err(|e| e.to_string())?;
    if std::fs::read_dir(&queue)
        .map_err(|e| e.to_string())?
        .take(65)
        .count()
        > 64
    {
        return Err("Manager sync is busy. Retry in a moment.".into());
    }
    store::write(&queue, &format!("{}.json", random(16)?), &action)
}
pub(super) fn start_worker(
    tx: tokio::sync::mpsc::Sender<super::ClientLoopEvent>,
    quit: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    service::start(tx, quit)
}
fn view_at(root: &Path, listening: bool) -> Result<View, String> {
    store::transaction(root, |state| {
        let pair = |p: &store::Pending| PairRequest {
            request: p.request.clone(),
            manager: p.manager.clone(),
            code: store::code(
                &p.token,
                &p.request,
                &state.id,
                &state.node,
                &p.manager.id,
                &p.manager.node,
            ),
            expires: p.expires,
            accepted: p.accepted == Some(true),
        };
        Ok(View {
            managers: state.discovered.clone(),
            paired: state
                .peers
                .iter()
                .map(|peer| Paired {
                    manager: peer.manager.clone(),
                    last_success: peer.last_success,
                    error: peer.error.clone(),
                })
                .collect(),
            incoming: state
                .incoming
                .iter()
                .filter(|p| {
                    p.accepted != Some(false)
                        && p.expires > now()
                        && !state.peers.iter().any(|peer| peer.pairing == p.request)
                })
                .map(pair)
                .collect(),
            outgoing: state
                .outgoing
                .as_ref()
                .filter(|p| p.expires > now() || p.confirmed)
                .map(pair),
            scanning: state.scanning,
            error: state.error.clone(),
            listening,
        })
    })
}
fn identity_matches(state: &store::State, network: &Network) -> Result<(), String> {
    if state.node == network.own.id {
        Ok(())
    } else {
        Err("Tailscale identity changed; retry pairing".into())
    }
}
fn approve_at(root: &Path, request: &str, accepted: bool) -> Result<(), String> {
    store::transaction(root, |state| {
        let index = state
            .incoming
            .iter()
            .position(|p| p.request == request)
            .ok_or("Pair request is no longer available")?;
        let pending = &state.incoming[index];
        if pending.expires <= now() {
            return Err("Pair request expired".into());
        }
        if pending.accepted.is_some()
            && !(pending.accepted == Some(true)
                && !accepted
                && !state.peers.iter().any(|peer| peer.pairing == request))
        {
            return Err("Pair request was already answered".into());
        }
        if accepted {
            if state.peers.len() >= store::MAX_PEERS {
                return Err("At most 16 managers can be paired".into());
            }
            let epoch = catalog_sync::direct_revision_at(root)?;
            state.incoming[index].epoch = Some(epoch);
        }
        state.incoming[index].accepted = Some(accepted);
        Ok(())
    })
}

pub(crate) fn disable_at(root: &Path) -> Result<(), String> {
    store::transaction(root, |s| {
        s.peers.clear();
        s.incoming.clear();
        s.outgoing = None;
        s.discovered.clear();
        s.error = None;
        catalog_sync::leave_direct_at(root)?;
        Ok(())
    })
}
pub(crate) fn disable() -> Result<(), String> {
    disable_at(&store::root())
}
