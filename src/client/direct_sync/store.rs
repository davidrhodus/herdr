use super::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::Path,
};

pub(super) const MAX_PEERS: usize = 16;
pub(super) const TTL: u64 = 180;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Peer {
    pub manager: Manager,
    pub token: String,
    pub pairing: String,
    pub last_success: Option<u64>,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    pub request: String,
    pub manager: Manager,
    pub token: String,
    pub expires: u64,
    pub accepted: Option<bool>,
    #[serde(default)]
    pub epoch: Option<String>,
    #[serde(default)]
    pub confirmed: bool,
    pub ticket: super::wire::Ticket,
    pub promotion: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Seen {
    pub request: String,
    pub expires: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    version: u32,
    pub id: String,
    pub node: String,
    pub peers: Vec<Peer>,
    pub incoming: Vec<Pending>,
    pub outgoing: Option<Pending>,
    pub discovered: Vec<Manager>,
    pub scanning: bool,
    #[serde(default)]
    pub seen: Vec<Seen>,
    pub admission_key: String,
    pub error: Option<String>,
}
impl State {
    fn new() -> Result<Self, String> {
        Ok(Self {
            version: 1,
            id: random(16)?,
            node: String::new(),
            peers: vec![],
            incoming: vec![],
            outgoing: None,
            discovered: vec![],
            scanning: false,
            seen: Vec::new(),
            admission_key: random(32)?,
            error: None,
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || !herdr_sync::valid_id(&self.id)
            || self.node.len() > 128
            || self.peers.len() > MAX_PEERS
            || self.incoming.len() > 8
            || self.discovered.len() > 256
            || self.seen.len() > 1024
            || self.seen.iter().any(|id| !secret(&id.request))
            || !secret(&self.admission_key)
        {
            return Err("Invalid manager pairing state".into());
        }
        let mut seen = std::collections::HashSet::new();
        for peer in &self.peers {
            peer.manager.validate()?;
            if !secret(&peer.token) || !secret(&peer.pairing) || !seen.insert(&peer.manager.id) {
                return Err("Invalid paired manager".into());
            }
        }
        for pending in self.incoming.iter().chain(self.outgoing.iter()) {
            pending.manager.validate()?;
            if !secret(&pending.token)
                || !secret(&pending.request)
                || !herdr_sync::valid_id(&pending.promotion)
            {
                return Err("Invalid pending pairing".into());
            }
        }
        for manager in &self.discovered {
            manager.validate()?;
        }
        Ok(())
    }
    pub fn active_peer(&self, id: &str, node: &str) -> Result<&Peer, String> {
        self.peers
            .iter()
            .find(|p| p.manager.id == id && p.manager.node == node)
            .ok_or_else(|| "Manager is not paired".into())
    }
}
pub(super) fn root() -> std::path::PathBuf {
    crate::config::state_dir().join("client")
}
pub(super) fn lock(root: &Path, name: &str) -> Result<File, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let path = root.join(name);
    match crate::platform::create_private_state_file(&path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(&path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("Pairing lock must not be a symlink".into());
            }
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}
pub(super) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(512 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 512 * 1024 {
        return Err("Manager sync state exceeds size limit".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "Invalid manager sync state".into())
}
pub(super) fn write<T: Serialize>(root: &Path, name: &str, value: &T) -> Result<(), String> {
    super::super::endpoint::store_private_json(
        &root.join(name),
        &serde_json::to_vec(value).map_err(|e| e.to_string())?,
        "manager sync",
    )
}
pub(super) fn transaction<T>(
    root: &Path,
    f: impl FnOnce(&mut State) -> Result<T, String>,
) -> Result<T, String> {
    let guard = lock(root, "direct-sync.lock")?;
    guard.lock().map_err(|e| e.to_string())?;
    let previous = read::<State>(&root.join("direct-sync.json"))?;
    let before = previous
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut state = previous.map(Ok).unwrap_or_else(State::new)?;
    state.validate()?;
    let result = f(&mut state)?;
    state.validate()?;
    if before.as_deref()
        != Some(
            serde_json::to_vec(&state)
                .map_err(|e| e.to_string())?
                .as_slice(),
        )
    {
        write(root, "direct-sync.json", &state)?;
    }
    Ok(result)
}
pub(super) fn bind_identity(root: &Path, node: &str) -> Result<(), String> {
    transaction(root, |state| {
        if !state.node.is_empty() && state.node != node {
            state.id = random(16)?;
            state.peers.clear();
            state.incoming.clear();
            state.outgoing = None;
            state.discovered.clear();
            state.error=Some("Tailscale identity changed. Pair your managers again; your server list is retained.".into());
        }
        state.node = node.into();
        Ok(())
    })
}
pub(super) fn code(
    token: &str,
    request: &str,
    a: &str,
    anode: &str,
    b: &str,
    bnode: &str,
) -> String {
    use sha2::Digest;
    let mut identities = [(a, anode), (b, bnode)];
    identities.sort();
    let mut hash = sha2::Sha256::new();
    hash.update(b"herdr-pair-fingerprint-v2");
    for value in [
        token,
        request,
        identities[0].0,
        identities[0].1,
        identities[1].0,
        identities[1].1,
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    let digest = hash.finalize();
    digest[..16]
        .chunks(2)
        .map(|chunk| format!("{:02x}{:02x}", chunk[0], chunk[1]))
        .collect::<Vec<_>>()
        .join("-")
}

pub(super) fn secret(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn random(count: usize) -> Result<String, String> {
    let mut bytes = vec![0u8; count];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub(super) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
