use super::*;
use herdr_sync::Document;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};
const MAX_PACKET: usize = herdr_sync::MAX_BODY + 4096;
const MAGIC: &[u8; 8] = b"HDRSYNC1";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Ticket {
    pub nonce: String,
    pub expires: u64,
    pub proof: Vec<u8>,
}
pub(super) fn ticket(state: &store::State, source: &str) -> Result<Ticket, String> {
    let nonce = random(32)?;
    let expires = now() + store::TTL;
    Ok(Ticket {
        proof: sign(
            &state.admission_key,
            "admission",
            &[&state.id, &state.node, source, &nonce, &expires.to_string()],
            None,
        )?,
        nonce,
        expires,
    })
}
fn verify_ticket(state: &store::State, source: &str, ticket: &Ticket) -> Result<(), String> {
    if ticket.expires <= now()
        || ticket.expires > now() + store::TTL
        || !store::secret(&ticket.nonce)
    {
        return Err("Pairing discovery expired. Refresh and retry.".into());
    }
    verify(
        &state.admission_key,
        "admission",
        &[
            &state.id,
            &state.node,
            source,
            &ticket.nonce,
            &ticket.expires.to_string(),
        ],
        None,
        &ticket.proof,
    )
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum Request {
    Probe,
    Begin {
        from: String,
        request: String,
        token: String,
        ticket: Ticket,
    },
    Poll {
        from: String,
        request: String,
        proof: Vec<u8>,
    },
    Finalize {
        from: String,
        request: String,
        proof: Vec<u8>,
    },
    Cancel {
        from: String,
        request: String,
        proof: Vec<u8>,
    },
    Auth {
        from: String,
        nonce: String,
    },
    Exchange {
        document: Document,
        proof: Vec<u8>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum Response {
    Hello {
        id: String,
        ticket: Ticket,
    },
    Cancelled,
    Pair {
        phase: String,
        proof: Vec<u8>,
    },
    Auth {
        id: String,
        nonce: String,
        proof: Vec<u8>,
    },
    Document {
        document: Document,
        proof: Vec<u8>,
    },
    Error {
        message: String,
    },
}
pub(super) struct Connection {
    stream: TcpStream,
    deadline: Instant,
}
impl Connection {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }
    pub fn connect(ip: IpAddr) -> Result<Self, String> {
        TcpStream::connect_timeout(&SocketAddr::new(ip, PORT), Duration::from_millis(300))
            .map(Self::new)
            .map_err(|_| {
                "Manager is unavailable. Keep both UIs open and check Tailscale access.".into()
            })
    }
    fn remaining(&self) -> Result<Duration, String> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err("Manager connection timed out".into())
        } else {
            Ok(remaining)
        }
    }
    pub fn send<T: Serialize>(&mut self, value: &T) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_PACKET {
            return Err("Manager sync message exceeds size limit".into());
        }
        self.write(MAGIC)?;
        self.write(&(bytes.len() as u32).to_be_bytes())?;
        self.write(&bytes)
    }
    fn write(&mut self, mut bytes: &[u8]) -> Result<(), String> {
        while !bytes.is_empty() {
            self.stream
                .set_write_timeout(Some(self.remaining()?))
                .map_err(|e| e.to_string())?;
            let count = self.stream.write(bytes).map_err(|e| e.to_string())?;
            if count == 0 {
                return Err("Manager closed the connection".into());
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }

    pub fn receive<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, String> {
        self.receive_limited(MAX_PACKET)
    }
    fn receive_limited<T: serde::de::DeserializeOwned>(
        &mut self,
        limit: usize,
    ) -> Result<T, String> {
        let mut header = [0u8; 12];
        self.read(&mut header)?;
        if &header[..8] != MAGIC {
            return Err("Not a compatible Herdr manager".into());
        }
        let len = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
        if len > limit {
            return Err("Manager sync message exceeds size limit".into());
        }
        let mut bytes = vec![0; len];
        self.read(&mut bytes)?;
        serde_json::from_slice(&bytes).map_err(|_| "Invalid manager sync message".into())
    }
    fn read(&mut self, mut bytes: &mut [u8]) -> Result<(), String> {
        while !bytes.is_empty() {
            self.stream
                .set_read_timeout(Some(self.remaining()?))
                .map_err(|e| e.to_string())?;
            let count = self.stream.read(bytes).map_err(|e| e.to_string())?;
            if count == 0 {
                return Err("Manager closed the connection".into());
            }
            bytes = &mut bytes[count..];
        }
        Ok(())
    }
}
fn mac(
    token: &str,
    label: &str,
    parts: &[&str],
    document: Option<&Document>,
) -> Result<Hmac<Sha256>, String> {
    if !store::secret(token) {
        return Err("Invalid pairing credential".into());
    }
    let mut mac =
        Hmac::<Sha256>::new_from_slice(token.as_bytes()).map_err(|_| "Invalid pairing key")?;
    mac.update(b"herdr-direct-sync-auth-v1");
    mac.update(&serde_json::to_vec(&(label, parts, document)).map_err(|e| e.to_string())?);
    Ok(mac)
}
pub(super) fn sign(
    token: &str,
    label: &str,
    parts: &[&str],
    doc: Option<&Document>,
) -> Result<Vec<u8>, String> {
    Ok(mac(token, label, parts, doc)?
        .finalize()
        .into_bytes()
        .to_vec())
}
pub(super) fn verify(
    token: &str,
    label: &str,
    parts: &[&str],
    doc: Option<&Document>,
    proof: &[u8],
) -> Result<(), String> {
    mac(token, label, parts, doc)?
        .verify_slice(proof)
        .map_err(|_| "Manager authentication failed".into())
}
pub(super) fn probe_details(ip: IpAddr) -> Result<(String, Ticket), String> {
    let mut c = Connection::connect(ip)?;
    c.send(&Request::Probe)?;
    match c.receive()? {
        Response::Hello { id, ticket } if herdr_sync::valid_id(&id) => Ok((id, ticket)),
        _ => Err("Not a compatible Herdr manager".into()),
    }
}
pub(super) fn probe(ip: IpAddr) -> Result<String, String> {
    probe_details(ip).map(|(id, _)| id)
}

pub(super) fn handle(
    root: &Path,
    c: Connection,
    network: &Network,
    source: &Node,
    ip: IpAddr,
) -> Result<(), String> {
    handle_with(root, c, network, source, ip, || {
        let fresh = super::super::machine_setup::tailscale::network()?;
        if fresh.own.id != network.own.id
            || !fresh
                .peers
                .iter()
                .any(|peer| peer.id == source.id && peer.ips.contains(&ip))
        {
            return Err("Tailscale identity changed during the request".into());
        }
        Ok(())
    })
}
pub(super) fn handle_with(
    root: &Path,
    mut c: Connection,
    network: &Network,
    source: &Node,
    ip: IpAddr,
    validate_network: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    let request: Request = c.receive_limited(16 * 1024)?;
    validate_network()?;
    match request {
        Request::Probe => {
            let (id, ticket) = store::transaction(root, |s| {
                identity_matches(s, network)?;
                Ok((s.id.clone(), ticket(s, &source.id)?))
            })?;
            c.send(&Response::Hello { id, ticket })
        }
        Request::Begin {
            from,
            request,
            token,
            ticket,
        } => {
            if !herdr_sync::valid_id(&from)
                || !store::secret(&request)
                || !store::secret(&token)
                || request != ticket.nonce
            {
                return Err("Invalid pairing request".into());
            }
            let phase = store::transaction(root, |s| {
                identity_matches(s, network)?;
                if from == s.id
                    || s.peers
                        .iter()
                        .any(|p| p.manager.id == from && p.manager.node != source.id)
                {
                    return Err("Manager identity conflicts with an existing pairing".into());
                }
                if let Some(peer) = s.peers.iter().find(|p| {
                    p.manager.id == from && p.manager.node == source.id && p.pairing == request
                }) {
                    if peer.token == token {
                        return Ok("accepted");
                    }
                    return Err("Invalid pairing request".into());
                }
                verify_ticket(s, &source.id, &ticket)?;
                if s.outgoing
                    .as_ref()
                    .is_some_and(|p| p.manager.id == from && p.manager.node == source.id)
                {
                    if s.id < from {
                        return Err("Both managers started pairing. Approve the request on the other computer.".into());
                    }
                    s.outgoing = None;
                }
                s.incoming.retain(|p| p.expires > now());
                s.seen.retain(|seen| seen.expires > now());
                if let Some(p) = s.incoming.iter().find(|p| p.request == request) {
                    if p.manager.id != from || p.manager.node != source.id || p.token != token {
                        return Err("Invalid pairing request".into());
                    }
                    return Ok(if p.accepted == Some(false) {
                        "declined"
                    } else if p.accepted == Some(true) {
                        "accepted"
                    } else {
                        "pending"
                    });
                }
                if s.incoming.len() >= 8
                    || s.incoming
                        .iter()
                        .any(|p| p.manager.node == source.id && p.accepted.is_none())
                {
                    return Err(
                        "A pairing request is already pending. Retry after it expires.".into(),
                    );
                }
                if s.seen.iter().any(|seen| seen.request == request) {
                    return Err("Pair request expired; start a new pairing".into());
                }
                if s.seen.len() >= 1024 {
                    return Err("Pairing request history limit reached".into());
                }
                s.seen.push(store::Seen {
                    request: request.clone(),
                    expires: ticket.expires,
                });
                s.incoming.push(store::Pending {
                    request: request.clone(),
                    manager: Manager {
                        id: from.clone(),
                        node: source.id.clone(),
                        name: source.name.clone(),
                        ip,
                    },
                    token: token.clone(),
                    expires: ticket.expires,
                    accepted: None,
                    epoch: None,
                    confirmed: false,
                    ticket: ticket.clone(),
                    promotion: random(16)?,
                });
                Ok("pending")
            })?;
            let id = store::transaction(root, |s| Ok(s.id.clone()))?;
            c.send(&Response::Pair {
                phase: phase.into(),
                proof: sign(&token, phase, &[&from, &id, &request], None)?,
            })
        }
        Request::Poll {
            from,
            request,
            proof,
        } => {
            let (phase, token, id) = store::transaction(root, |s| {
                identity_matches(s, network)?;
                if let Some(p) = s.peers.iter().find(|p| {
                    p.manager.id == from && p.manager.node == source.id && p.pairing == request
                }) {
                    verify(&p.token, "poll", &[&from, &s.id, &request], None, &proof)?;
                    return Ok(("accepted", p.token.clone(), s.id.clone()));
                }
                let p = s
                    .incoming
                    .iter()
                    .find(|p| {
                        p.manager.id == from
                            && p.manager.node == source.id
                            && p.request == request
                            && p.expires > now()
                    })
                    .ok_or("Pair request expired or was declined")?;
                verify(&p.token, "poll", &[&from, &s.id, &request], None, &proof)?;
                Ok((
                    if p.accepted == Some(false) {
                        "declined"
                    } else if p.accepted == Some(true) {
                        "accepted"
                    } else {
                        "pending"
                    },
                    p.token.clone(),
                    s.id.clone(),
                ))
            })?;
            c.send(&Response::Pair {
                phase: phase.into(),
                proof: sign(&token, phase, &[&from, &id, &request], None)?,
            })
        }
        Request::Finalize {
            from,
            request,
            proof,
        } => {
            let (token, own, phase) = store::transaction(root, |s| {
                identity_matches(s, network)?;
                if let Some(peer) = s.peers.iter().find(|p| {
                    p.manager.id == from && p.manager.node == source.id && p.pairing == request
                }) {
                    verify(
                        &peer.token,
                        "finalize",
                        &[&from, &s.id, &request],
                        None,
                        &proof,
                    )?;
                    return Ok((peer.token.clone(), s.id.clone(), "finalized"));
                }
                let pending = s
                    .incoming
                    .iter()
                    .find(|p| {
                        p.manager.id == from && p.manager.node == source.id && p.request == request
                    })
                    .ok_or("Pairing approval expired or was revoked")?
                    .clone();
                verify(
                    &pending.token,
                    "finalize",
                    &[&from, &s.id, &request],
                    None,
                    &proof,
                )?;
                if pending.accepted != Some(true) || pending.expires <= now() {
                    return Ok((pending.token, s.id.clone(), "expired"));
                }
                if let Err(error) = catalog_sync::enable_direct_when(
                    root,
                    pending.epoch.as_deref(),
                    &pending.promotion,
                ) {
                    if error.starts_with("Sync configuration changed") {
                        return Ok((pending.token, s.id.clone(), "changed"));
                    }
                    return Err(error);
                }
                if s.peers.len() >= store::MAX_PEERS
                    && !s.peers.iter().any(|p| p.manager.id == from)
                {
                    return Err("At most 16 managers can be paired".into());
                }
                s.peers.retain(|p| p.manager.id != from);
                s.peers.push(store::Peer {
                    manager: pending.manager,
                    token: pending.token.clone(),
                    pairing: request.clone(),
                    last_success: None,
                    error: None,
                });
                s.incoming.retain(|p| p.manager.id != from);
                Ok((pending.token, s.id.clone(), "finalized"))
            })?;
            c.send(&Response::Pair {
                phase: phase.into(),
                proof: sign(&token, phase, &[&from, &own, &request], None)?,
            })
        }
        Request::Cancel {
            from,
            request,
            proof,
        } => {
            store::transaction(root, |s| {
                identity_matches(s, network)?;
                let token = s
                    .peers
                    .iter()
                    .find(|p| {
                        p.manager.id == from && p.manager.node == source.id && p.pairing == request
                    })
                    .map(|p| &p.token)
                    .or_else(|| {
                        s.incoming
                            .iter()
                            .find(|p| {
                                p.manager.id == from
                                    && p.manager.node == source.id
                                    && p.request == request
                            })
                            .map(|p| &p.token)
                    })
                    .ok_or("Unknown pairing request")?;
                verify(token, "cancel", &[&from, &s.id, &request], None, &proof)?;
                s.peers.retain(|p| p.pairing != request);
                s.incoming.retain(|p| p.request != request);
                Ok(())
            })?;
            c.send(&Response::Cancelled)
        }
        Request::Auth {
            from,
            nonce: client_nonce,
        } => {
            if !store::secret(&client_nonce) {
                return Err("Invalid manager challenge".into());
            }
            let (own, peer, epoch) = store::transaction(root, |s| {
                identity_matches(s, network)?;
                let p = s.active_peer(&from, &source.id)?.clone();
                let (epoch, _) = catalog_sync::direct_snapshot_at(root)?;
                Ok((s.id.clone(), p, epoch))
            })?;
            let server_nonce = random(32)?;
            let parts = [
                from.as_str(),
                own.as_str(),
                client_nonce.as_str(),
                server_nonce.as_str(),
            ];
            c.send(&Response::Auth {
                id: own.clone(),
                nonce: server_nonce.clone(),
                proof: sign(&peer.token, "server", &parts, None)?,
            })?;
            let Request::Exchange { document, proof } = c.receive()? else {
                return Err("Expected authenticated catalog exchange".into());
            };
            verify(&peer.token, "request", &parts, Some(&document), &proof)?;
            validate_network()?;
            let merged = store::transaction(root, |s| {
                identity_matches(s, network)?;
                let current = s.active_peer(&from, &source.id)?;
                if current.token != peer.token || current.pairing != peer.pairing {
                    return Err("Pairing was revoked".into());
                }
                let merged = catalog_sync::direct_merge_at(root, &epoch, &document)?;
                if let Some(p) = s.peers.iter_mut().find(|p| p.manager.id == from) {
                    p.last_success = Some(now());
                    p.error = None;
                    p.manager.name = source.name.clone();
                    p.manager.ip = ip;
                }
                Ok(merged)
            })?;
            c.send(&Response::Document {
                proof: sign(&peer.token, "response", &parts, Some(&merged))?,
                document: merged,
            })
        }
        Request::Exchange { .. } => Err("Authenticate before exchanging server lists".into()),
    }
}

pub(super) fn sync_peer(root: &Path, _network: &Network, peer: &store::Peer) -> Result<(), String> {
    let network = super::super::machine_setup::tailscale::network()?;
    sync_peer_checked(root, &network, peer, Connection::connect, || {
        let fresh = super::super::machine_setup::tailscale::network()?;
        if fresh.own.id != network.own.id {
            return Err("Tailscale identity changed while syncing".into());
        }
        Ok(())
    })
}
#[cfg(test)]
pub(super) fn sync_peer_with(
    root: &Path,
    network: &Network,
    peer: &store::Peer,
    connect: impl Fn(IpAddr) -> Result<Connection, String>,
) -> Result<(), String> {
    sync_peer_checked(root, network, peer, connect, || Ok(()))
}
fn sync_peer_checked(
    root: &Path,
    network: &Network,
    peer: &store::Peer,
    connect: impl Fn(IpAddr) -> Result<Connection, String>,
    validate_network: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    let (own, epoch, document) = store::transaction(root, |s| {
        identity_matches(s, network)?;
        let current = s.active_peer(&peer.manager.id, &peer.manager.node)?;
        if current.token != peer.token || current.pairing != peer.pairing {
            return Err("Pairing was revoked".into());
        }
        let (epoch, doc) = catalog_sync::direct_snapshot_at(root)?;
        Ok((s.id.clone(), epoch, doc))
    })?;
    let node = network
        .peers
        .iter()
        .find(|n| n.id == peer.manager.node && n.online)
        .ok_or("Paired manager is offline")?;
    let cn = random(32)?;
    let mut authenticated = None;
    for ip in peer_addresses(node, peer.manager.ip) {
        let attempt = (|| {
            let mut c = connect(ip)?;
            c.send(&Request::Auth {
                from: own.clone(),
                nonce: cn.clone(),
            })?;
            let Response::Auth {
                id,
                nonce: sn,
                proof,
            } = c.receive()?
            else {
                return Err("Manager did not authenticate".to_string());
            };
            if id != peer.manager.id || !store::secret(&sn) {
                return Err("Manager identity changed".into());
            }
            verify(&peer.token, "server", &[&own, &id, &cn, &sn], None, &proof)?;
            Ok((c, id, sn, ip))
        })();
        if let Ok(connection) = attempt {
            authenticated = Some(connection);
            break;
        }
    }
    let (mut c, id, sn, verified_ip) = authenticated
        .ok_or("Paired manager is unavailable or did not authenticate; keep both UIs open")?;
    let parts = [own.as_str(), id.as_str(), cn.as_str(), sn.as_str()];
    c.send(&Request::Exchange {
        proof: sign(&peer.token, "request", &parts, Some(&document))?,
        document,
    })?;
    let Response::Document { document, proof } = c.receive()? else {
        return Err("Paired manager rejected the exchange".into());
    };
    verify(&peer.token, "response", &parts, Some(&document), &proof)?;
    validate_network()?;
    store::transaction(root, |s| {
        identity_matches(s, network)?;
        let current = s.active_peer(&id, &peer.manager.node)?;
        if current.token != peer.token || current.pairing != peer.pairing {
            return Err("Pairing was revoked while syncing".into());
        }
        catalog_sync::direct_merge_at(root, &epoch, &document)?;
        if let Some(p) = s.peers.iter_mut().find(|p| p.manager.id == id) {
            p.last_success = Some(now());
            p.error = None;
            p.manager.name = node.name.clone();
            p.manager.ip = verified_ip;
        }
        Ok(())
    })
}

fn peer_addresses(node: &Node, preferred: IpAddr) -> Vec<IpAddr> {
    let mut ips = node.ips.clone();
    ips.sort_by_key(|ip| *ip != preferred);
    ips
}
pub(super) fn finalize_pending(
    root: &Path,
    network: &Network,
    pending: &store::Pending,
) -> Result<(), String> {
    finalize_pending_with(root, network, pending, Connection::connect)
}
pub(super) fn finalize_pending_with(
    root: &Path,
    network: &Network,
    pending: &store::Pending,
    connect: impl Fn(IpAddr) -> Result<Connection, String>,
) -> Result<(), String> {
    let own = store::transaction(root, |s| {
        identity_matches(s, network)?;
        if !s.outgoing.as_ref().is_some_and(|p| {
            p.request == pending.request && p.token == pending.token && p.confirmed
        }) {
            return Err("Pair request changed".into());
        }
        Ok(s.id.clone())
    })?;
    let node = network
        .peers
        .iter()
        .find(|n| n.id == pending.manager.node && n.online)
        .ok_or("Manager is offline")?;
    let mut completed = false;
    for ip in peer_addresses(node, pending.manager.ip) {
        let result = (|| {
            let mut c = connect(ip)?;
            c.send(&Request::Finalize {
                from: own.clone(),
                request: pending.request.clone(),
                proof: sign(
                    &pending.token,
                    "finalize",
                    &[&own, &pending.manager.id, &pending.request],
                    None,
                )?,
            })?;
            let Response::Pair { phase, proof } = c.receive()? else {
                return Err("Manager rejected pairing".to_string());
            };
            verify(
                &pending.token,
                &phase,
                &[&own, &pending.manager.id, &pending.request],
                None,
                &proof,
            )?;
            if phase == "expired" || phase == "changed" {
                return Err("Pairing approval expired or changed. Start pairing again.".into());
            }
            if phase != "finalized" {
                return Err("Manager has not finalized pairing".into());
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                completed = true;
                break;
            }
            Err(error) if error.starts_with("Pairing approval expired") => return Err(error),
            Err(_) => {}
        }
    }
    if !completed {
        return Err("Manager has not finalized pairing. Keep both UIs open.".into());
    }
    store::transaction(root, |s| {
        identity_matches(s, network)?;
        let current = s
            .outgoing
            .as_ref()
            .filter(|p| p.request == pending.request && p.token == pending.token && p.confirmed)
            .ok_or("Pair request was cancelled")?;
        catalog_sync::enable_direct_when(root, current.epoch.as_deref(), &current.promotion)?;
        if s.peers.len() >= store::MAX_PEERS
            && !s.peers.iter().any(|p| p.manager.id == pending.manager.id)
        {
            return Err("At most 16 managers can be paired".into());
        }
        s.peers.retain(|p| p.manager.id != pending.manager.id);
        s.peers.push(store::Peer {
            manager: pending.manager.clone(),
            token: pending.token.clone(),
            pairing: pending.request.clone(),
            last_success: None,
            error: None,
        });
        s.outgoing = None;
        s.error = None;
        Ok(())
    })
}
