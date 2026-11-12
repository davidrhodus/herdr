use super::super::endpoint::{EndpointCatalog, SavedSshEndpoint};
use super::*;
use herdr_sync::Document;
use std::{
    net::{TcpListener, TcpStream},
    path::PathBuf,
    thread,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        Self(crate::platform::create_remote_ssh_config_dir("direct-sync-test").unwrap())
    }
    fn catalog(&self) -> EndpointCatalog {
        store::read(&self.0.join("endpoints.json"))
            .unwrap()
            .unwrap_or_default()
    }
    fn set(&self, catalog: &EndpointCatalog) {
        store::write(&self.0, "endpoints.json", catalog).unwrap();
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn manager(root: &Root, node: &str, ip: &str) -> Manager {
    store::bind_identity(&root.0, node).unwrap();
    let id = store::transaction(&root.0, |s| Ok(s.id.clone())).unwrap();
    Manager {
        id,
        node: node.into(),
        name: node.into(),
        ip: ip.parse().unwrap(),
    }
}
fn node(manager: &Manager) -> Node {
    Node {
        id: manager.node.clone(),
        name: manager.name.clone(),
        ips: vec![manager.ip],
        online: true,
    }
}
fn net(own: &Manager, other: &Manager) -> Network {
    Network {
        own: node(own),
        peers: vec![node(other)],
    }
}
fn peer(root: &Root, manager: &Manager, token: &str, pairing: &str) -> store::Peer {
    let peer = store::Peer {
        manager: manager.clone(),
        token: token.into(),
        pairing: pairing.into(),
        last_success: None,
        error: None,
    };
    store::transaction(&root.0, |s| {
        s.peers.push(peer.clone());
        catalog_sync::enable_direct_at(&root.0)
    })
    .unwrap();
    peer
}
fn serve(
    root: &Root,
    network: Network,
    source: Node,
) -> (std::net::SocketAddr, thread::JoinHandle<Result<(), String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let root = root.0.clone();
    let thread = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        wire::handle_with(
            &root,
            wire::Connection::new(stream),
            &network,
            &source,
            source.ips[0],
            || Ok(()),
        )
    });
    (address, thread)
}
fn ticket_for(root: &Root, source: &str) -> wire::Ticket {
    store::transaction(&root.0, |s| wire::ticket(s, source)).unwrap()
}
fn rpc(root: &Root, network: Network, source: Node, request: wire::Request) -> wire::Response {
    let (addr, server) = serve(root, network, source);
    let mut c = wire::Connection::new(TcpStream::connect(addr).unwrap());
    c.send(&request).unwrap();
    let reply = c.receive().unwrap();
    server.join().unwrap().unwrap();
    reply
}
fn add(root: &Root, label: &str) -> SavedSshEndpoint {
    let mut catalog = root.catalog();
    let id = catalog
        .add_ssh(label, format!("{label}.example.invalid"), "main")
        .unwrap();
    let profile = catalog.ssh.iter().find(|p| p.id == id).unwrap().clone();
    root.set(&catalog);
    profile
}
fn sync_to(a: &Root, an: &Network, p: &store::Peer, b: &Root, bn: Network) {
    let (addr, server) = serve(b, bn, an.own.clone());
    wire::sync_peer_with(&a.0, an, p, |_| {
        Ok(wire::Connection::new(TcpStream::connect(addr).unwrap()))
    })
    .unwrap();
    server.join().unwrap().unwrap();
}

#[test]
fn direct_sync_pairing_requires_both_approvals_then_merges_offline_edits_and_deletions() {
    let a = Root::new();
    let b = Root::new();
    let am = manager(&a, "node-a", "100.64.0.1");
    let bm = manager(&b, "node-b", "100.64.0.2");
    let an = net(&am, &bm);
    let bn = net(&bm, &am);
    let first = add(&a, "desktop");
    let second = add(&b, "laptop");
    let token = random(32).unwrap();
    let ticket = ticket_for(&b, &am.node);
    let request = ticket.nonce.clone();
    store::transaction(&a.0, |s| {
        s.outgoing = Some(store::Pending {
            request: request.clone(),
            manager: bm.clone(),
            token: token.clone(),
            expires: now() + 180,
            accepted: None,
            epoch: None,
            confirmed: false,
            ticket: ticket.clone(),
            promotion: random(16).unwrap(),
        });
        Ok(())
    })
    .unwrap();
    let response = rpc(
        &b,
        bn.clone(),
        node(&am),
        wire::Request::Begin {
            from: am.id.clone(),
            request: request.clone(),
            token: token.clone(),
            ticket: ticket.clone(),
        },
    );
    assert!(matches!(response,wire::Response::Pair{phase,..} if phase=="pending"));
    assert_eq!(
        view_at(&a.0, true).unwrap().outgoing.unwrap().code,
        view_at(&b.0, true).unwrap().incoming[0].code
    );
    assert!(catalog_sync::direct_snapshot_at(&b.0).is_err());
    assert!(service::confirm(&a.0, &request).is_err());
    approve_at(&b.0, &request, true).unwrap();
    assert!(approve_at(&b.0, &request, true).is_err());
    // Approval survives the initiator disappearing and resuming the same request.
    let response = rpc(
        &b,
        bn.clone(),
        node(&am),
        wire::Request::Poll {
            from: am.id.clone(),
            request: request.clone(),
            proof: wire::sign(&token, "poll", &[&am.id, &bm.id, &request], None).unwrap(),
        },
    );
    match response {
        wire::Response::Pair { phase, proof } => {
            assert_eq!(phase, "accepted");
            wire::verify(&token, &phase, &[&am.id, &bm.id, &request], None, &proof).unwrap();
        }
        _ => panic!(),
    }
    store::transaction(&a.0, |s| {
        s.outgoing.as_mut().unwrap().accepted = Some(true);
        Ok(())
    })
    .unwrap();
    service::confirm(&a.0, &request).unwrap();
    assert!(store::transaction(&b.0, |s| Ok(s.peers.is_empty())).unwrap());
    let pending = store::transaction(&a.0, |s| Ok(s.outgoing.clone().unwrap())).unwrap();
    catalog_sync::enable_direct_when(&a.0, pending.epoch.as_deref(), &pending.promotion).unwrap();
    let (addr, server) = serve(&b, bn.clone(), node(&am));
    wire::finalize_pending_with(&a.0, &an, &pending, |_| {
        Ok(wire::Connection::new(TcpStream::connect(addr).unwrap()))
    })
    .unwrap();
    server.join().unwrap().unwrap();
    let ap = store::transaction(&a.0, |s| Ok(s.peers[0].clone())).unwrap();
    sync_to(&a, &an, &ap, &b, bn.clone());
    assert_eq!(a.catalog().ssh.len(), 2);
    assert_eq!(b.catalog().ssh.len(), 2);
    let mut ac = a.catalog();
    ac.rename_ssh(&first.id, "Renamed").unwrap();
    ac.remove_ssh(&second.id);
    a.set(&ac);
    add(&b, "new-offline");
    sync_to(&a, &an, &ap, &b, bn);
    for root in [&a, &b] {
        let catalog = root.catalog();
        assert_eq!(catalog.ssh.len(), 2);
        assert!(catalog.ssh.iter().any(|p| p.label == "Renamed"));
        assert!(catalog.ssh.iter().any(|p| p.label == "new-offline"));
        assert!(!catalog.ssh.iter().any(|p| p.id == second.id));
    }
}
#[test]
fn direct_sync_declined_and_expired_requests_cannot_be_reapproved_or_replayed() {
    let a = Root::new();
    let b = Root::new();
    let am = manager(&a, "a", "100.64.0.1");
    let bm = manager(&b, "b", "100.64.0.2");
    let bn = net(&bm, &am);
    let token = random(32).unwrap();
    let ticket = ticket_for(&b, &am.node);
    let request = ticket.nonce.clone();
    rpc(
        &b,
        bn.clone(),
        node(&am),
        wire::Request::Begin {
            from: am.id.clone(),
            request: request.clone(),
            token: token.clone(),
            ticket: ticket.clone(),
        },
    );
    approve_at(&b.0, &request, false).unwrap();
    assert!(approve_at(&b.0, &request, true).is_err());
    store::transaction(&b.0, |s| {
        s.incoming[0].expires = now() - 1;
        Ok(())
    })
    .unwrap();
    assert!(approve_at(&b.0, &request, true).is_err());
    let (addr, server) = serve(&b, bn, node(&am));
    let mut c = wire::Connection::new(TcpStream::connect(addr).unwrap());
    c.send(&wire::Request::Begin {
        from: am.id,
        request,
        token,
        ticket: ticket_for(&b, &am.node),
    })
    .unwrap();
    assert!(c.receive::<wire::Response>().is_err());
    assert!(server.join().unwrap().is_err());
    assert!(store::transaction(&b.0, |s| Ok(s.peers.is_empty())).unwrap());
    assert!(catalog_sync::direct_snapshot_at(&b.0).is_err());
}
#[test]
fn direct_sync_incoming_revocation_and_mode_changes_fence_authenticated_in_flight_requests() {
    for change_mode in [false, true] {
        let a = Root::new();
        let b = Root::new();
        let am = manager(&a, "a", "100.64.0.1");
        let bm = manager(&b, "b", "100.64.0.2");
        let bn = net(&bm, &am);
        let token = random(32).unwrap();
        let pairing = random(32).unwrap();
        peer(&b, &am, &token, &pairing);
        add(&a, "never-publish");
        catalog_sync::enable_direct_at(&a.0).unwrap();
        let (_, doc) = catalog_sync::direct_snapshot_at(&a.0).unwrap();
        let (addr, server) = serve(&b, bn, node(&am));
        let mut c = wire::Connection::new(TcpStream::connect(addr).unwrap());
        let cn = random(32).unwrap();
        c.send(&wire::Request::Auth {
            from: am.id.clone(),
            nonce: cn.clone(),
        })
        .unwrap();
        let wire::Response::Auth { nonce: sn, .. } = c.receive().unwrap() else {
            panic!()
        };
        if change_mode {
            catalog_sync::leave_at(&b.0).unwrap();
            catalog_sync::enable_direct_at(&b.0).unwrap();
        } else {
            store::transaction(&b.0, |s| {
                s.peers.clear();
                Ok(())
            })
            .unwrap();
        }
        c.send(&wire::Request::Exchange {
            proof: wire::sign(&token, "request", &[&am.id, &bm.id, &cn, &sn], Some(&doc)).unwrap(),
            document: doc,
        })
        .unwrap();
        assert!(c.receive::<wire::Response>().is_err());
        assert!(server.join().unwrap().is_err());
        assert!(b.catalog().ssh.is_empty());
    }
}
#[test]
fn direct_sync_outbound_revocation_during_network_io_prevents_local_publication() {
    let a = Root::new();
    let b = Root::new();
    let am = manager(&a, "a", "100.64.0.1");
    let bm = manager(&b, "b", "100.64.0.2");
    let an = net(&am, &bm);
    let bn = net(&bm, &am);
    let token = random(32).unwrap();
    let pairing = random(32).unwrap();
    let ap = peer(&a, &bm, &token, &pairing);
    peer(&b, &am, &token, &pairing);
    add(&b, "not-on-revoked-a");
    let (addr, server) = serve(&b, bn, node(&am));
    let result = wire::sync_peer_with(&a.0, &an, &ap, |_| {
        store::transaction(&a.0, |s| {
            s.peers.clear();
            Ok(())
        })
        .unwrap();
        Ok(wire::Connection::new(TcpStream::connect(addr).unwrap()))
    });
    assert!(result.is_err());
    server.join().unwrap().unwrap();
    assert!(a.catalog().ssh.is_empty());
}
#[test]
fn direct_sync_hmac_binds_identity_nonce_direction_and_catalog() {
    let token = random(32).unwrap();
    let parts = ["from", "to", "client", "server"];
    let doc = Document::default();
    let proof = wire::sign(&token, "request", &parts, Some(&doc)).unwrap();
    wire::verify(&token, "request", &parts, Some(&doc), &proof).unwrap();
    assert!(wire::verify(&token, "response", &parts, Some(&doc), &proof).is_err());
    assert!(wire::verify(
        &token,
        "request",
        &["from", "other", "client", "server"],
        Some(&doc),
        &proof
    )
    .is_err());
    assert!(wire::verify(&random(32).unwrap(), "request", &parts, Some(&doc), &proof).is_err());
}
#[test]
fn direct_sync_identity_changes_revoke_all_pairings_and_only_one_local_owner_can_listen() {
    let a = Root::new();
    let b = Root::new();
    let am = manager(&a, "old-node", "100.64.0.1");
    let bm = manager(&b, "other", "100.64.0.2");
    peer(&a, &bm, &random(32).unwrap(), &random(32).unwrap());
    store::bind_identity(&a.0, "new-node").unwrap();
    store::transaction(&a.0, |s| {
        assert_ne!(s.id, am.id);
        assert!(s.peers.is_empty());
        Ok(())
    })
    .unwrap();
    let first = store::lock(&a.0, "owner-test.lock").unwrap();
    first.try_lock().unwrap();
    let second = store::lock(&a.0, "owner-test.lock").unwrap();
    assert!(second.try_lock().is_err());
    drop(first);
    second.try_lock().unwrap();
}
#[test]
fn direct_sync_framing_rejects_oversized_messages_before_payload_allocation() {
    use std::io::Write;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"HDRSYNC1").unwrap();
        stream.write_all(&u32::MAX.to_be_bytes()).unwrap();
    });
    let mut c = wire::Connection::new(TcpStream::connect(addr).unwrap());
    assert!(c
        .receive::<wire::Response>()
        .err()
        .unwrap()
        .contains("size limit"));
    server.join().unwrap();
}

fn rejected_rpc(root: &Root, network: Network, source: Node, request: wire::Request) {
    let (addr, server) = serve(root, network, source);
    let mut c = wire::Connection::new(TcpStream::connect(addr).unwrap());
    c.send(&request).unwrap();
    assert!(c.receive::<wire::Response>().is_err());
    assert!(server.join().unwrap().is_err());
}
#[test]
fn direct_sync_provisional_approval_expires_without_creating_a_trusted_peer() {
    let a = Root::new();
    let b = Root::new();
    let am = manager(&a, "a", "100.64.0.1");
    let bm = manager(&b, "b", "100.64.0.2");
    let bn = net(&bm, &am);
    let token = random(32).unwrap();
    let ticket = ticket_for(&b, &am.node);
    let request = ticket.nonce.clone();
    rpc(
        &b,
        bn.clone(),
        node(&am),
        wire::Request::Begin {
            from: am.id.clone(),
            request: request.clone(),
            token: token.clone(),
            ticket,
        },
    );
    approve_at(&b.0, &request, true).unwrap();
    assert!(view_at(&b.0, true).unwrap().incoming[0].accepted);
    assert!(store::transaction(&b.0, |s| Ok(s.peers.is_empty())).unwrap());
    rejected_rpc(
        &b,
        bn.clone(),
        node(&am),
        wire::Request::Auth {
            from: am.id.clone(),
            nonce: random(32).unwrap(),
        },
    );
    store::transaction(&b.0, |s| {
        s.incoming[0].expires = now() - 1;
        Ok(())
    })
    .unwrap();
    let expired = rpc(
        &b,
        bn,
        node(&am),
        wire::Request::Finalize {
            from: am.id.clone(),
            request: request.clone(),
            proof: wire::sign(&token, "finalize", &[&am.id, &bm.id, &request], None).unwrap(),
        },
    );
    assert!(matches!(expired,wire::Response::Pair{phase,..} if phase=="expired"));
    assert!(store::transaction(&b.0, |s| Ok(s.peers.is_empty())).unwrap());
}
#[test]
fn direct_sync_finalize_is_recoverable_but_cannot_resurrect_a_forgotten_or_replaced_pairing() {
    let a = Root::new();
    let b = Root::new();
    let am = manager(&a, "a", "100.64.0.1");
    let bm = manager(&b, "b", "100.64.0.2");
    let bn = net(&bm, &am);
    let mut old: Option<(String, String)> = None;
    for _ in 0..2 {
        let token = random(32).unwrap();
        let ticket = ticket_for(&b, &am.node);
        let request = ticket.nonce.clone();
        rpc(
            &b,
            bn.clone(),
            node(&am),
            wire::Request::Begin {
                from: am.id.clone(),
                request: request.clone(),
                token: token.clone(),
                ticket,
            },
        );
        approve_at(&b.0, &request, true).unwrap();
        let finalize = || wire::Request::Finalize {
            from: am.id.clone(),
            request: request.clone(),
            proof: wire::sign(&token, "finalize", &[&am.id, &bm.id, &request], None).unwrap(),
        };
        // The requester may have crashed after the receiver persisted finalization.
        rpc(&b, bn.clone(), node(&am), finalize());
        rpc(&b, bn.clone(), node(&am), finalize());
        assert!(store::transaction(&b.0, |s| Ok(s.incoming.is_empty())).unwrap());
        if let Some((old_request, old_token)) = old {
            rejected_rpc(
                &b,
                bn.clone(),
                node(&am),
                wire::Request::Finalize {
                    from: am.id.clone(),
                    request: old_request.clone(),
                    proof: wire::sign(
                        &old_token,
                        "finalize",
                        &[&am.id, &bm.id, &old_request],
                        None,
                    )
                    .unwrap(),
                },
            );
        }
        old = Some((request, token));
    }
    service::forget(&b.0, &am.id).unwrap();
    let (request, token) = old.unwrap();
    rejected_rpc(
        &b,
        bn,
        node(&am),
        wire::Request::Finalize {
            from: am.id.clone(),
            request: request.clone(),
            proof: wire::sign(&token, "finalize", &[&am.id, &bm.id, &request], None).unwrap(),
        },
    );
    assert!(store::transaction(&b.0, |s| Ok(s.peers.is_empty())).unwrap());
}
#[test]
fn direct_sync_fingerprint_binds_verified_node_identities_and_resists_transparent_relays() {
    let token = random(32).unwrap();
    let request = random(32).unwrap();
    let honest = store::code(
        &token,
        &request,
        "manager-a",
        "node-a",
        "manager-b",
        "node-b",
    );
    assert_eq!(
        honest,
        store::code(
            &token,
            &request,
            "manager-b",
            "node-b",
            "manager-a",
            "node-a"
        )
    );
    let a_relay = store::code(
        &token,
        &request,
        "manager-a",
        "node-a",
        "manager-b",
        "node-c",
    );
    let b_relay = store::code(
        &token,
        &request,
        "manager-b",
        "node-b",
        "manager-a",
        "node-c",
    );
    assert_ne!(a_relay, b_relay);
    assert_ne!(honest, a_relay);
    assert_eq!(honest.chars().filter(|c| *c != '-').count(), 32); // 128-bit fingerprint, not an offline-grindable short PIN.
}
#[test]
fn direct_sync_expired_request_cannot_use_a_fresh_ticket_and_old_history_does_not_block_new_pairs()
{
    let a = Root::new();
    let b = Root::new();
    let am = manager(&a, "a", "100.64.0.1");
    let bm = manager(&b, "b", "100.64.0.2");
    let bn = net(&bm, &am);
    let token = random(32).unwrap();
    let old = random(32).unwrap();
    store::transaction(&b.0, |s| {
        s.seen = (0..1024)
            .map(|_| store::Seen {
                request: random(32).unwrap(),
                expires: now() - 1,
            })
            .collect();
        s.seen[0].request = old.clone();
        Ok(())
    })
    .unwrap();
    let fresh = ticket_for(&b, &am.node);
    rejected_rpc(
        &b,
        bn.clone(),
        node(&am),
        wire::Request::Begin {
            from: am.id.clone(),
            request: old,
            token: token.clone(),
            ticket: fresh.clone(),
        },
    );
    rpc(
        &b,
        bn,
        node(&am),
        wire::Request::Begin {
            from: am.id.clone(),
            request: fresh.nonce.clone(),
            token,
            ticket: fresh,
        },
    );
    assert_eq!(store::transaction(&b.0, |s| Ok(s.seen.len())).unwrap(), 1);
}
#[test]
fn direct_sync_stopping_direct_preserves_existing_ssh_and_host_modes() {
    for source in [
        serde_json::json!({"kind":"local"}),
        serde_json::json!({"kind":"ssh","target":"you@host"}),
    ] {
        let root = Root::new();
        let state = serde_json::json!({"version":1,"source":source,"device":"11111111111111111111111111111111","document":Document::default(),"baseline":[],"last_success":null,"last_error":null});
        store::write(&root.0, "catalog-sync.json", &state).unwrap();
        disable_at(&root.0).unwrap();
        assert_eq!(
            store::read::<serde_json::Value>(&root.0.join("catalog-sync.json"))
                .unwrap()
                .unwrap(),
            state
        );
    }
}
#[test]
fn direct_sync_tries_another_address_without_sending_catalog_to_an_unauthenticated_service() {
    let a = Root::new();
    let b = Root::new();
    let am = manager(&a, "a", "100.64.0.1");
    let bm = manager(&b, "b", "100.64.0.2");
    let mut an = net(&am, &bm);
    an.peers[0].ips.push("fd7a:115c:a1e0::2".parse().unwrap());
    let bn = net(&bm, &am);
    let token = random(32).unwrap();
    let pairing = random(32).unwrap();
    let ap = peer(&a, &bm, &token, &pairing);
    peer(&b, &am, &token, &pairing);
    add(&b, "private-server");
    let wrong = TcpListener::bind("127.0.0.1:0").unwrap();
    let wrong_addr = wrong.local_addr().unwrap();
    let wrong_id = bm.id.clone();
    let wrong_server = thread::spawn(move || {
        let (stream, _) = wrong.accept().unwrap();
        let mut c = wire::Connection::new(stream);
        let _: wire::Request = c.receive().unwrap();
        c.send(&wire::Response::Auth {
            id: wrong_id,
            nonce: random(32).unwrap(),
            proof: vec![0; 32],
        })
        .unwrap();
        assert!(c.receive::<wire::Request>().is_err());
    });
    let (addr, server) = serve(&b, bn, node(&am));
    let attempts = std::sync::atomic::AtomicUsize::new(0);
    wire::sync_peer_with(&a.0, &an, &ap, |_| {
        let addr = if attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
            wrong_addr
        } else {
            addr
        };
        Ok(wire::Connection::new(TcpStream::connect(addr).unwrap()))
    })
    .unwrap();
    server.join().unwrap().unwrap();
    wrong_server.join().unwrap();
    assert_eq!(a.catalog().ssh[0].label, "private-server");
}
#[test]
fn direct_sync_listener_can_restart_after_serving_a_connection_and_view_never_contains_credentials()
{
    use std::io::Read;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let client = thread::spawn(move || {
        let mut c = TcpStream::connect(addr).unwrap();
        let mut b = [0; 1];
        assert_eq!(c.read(&mut b).unwrap(), 0);
    });
    let (stream, _) = listener.accept().unwrap();
    drop(stream);
    client.join().unwrap();
    drop(listener);
    let rebound = TcpListener::bind(addr).unwrap();
    drop(rebound);
    let a = Root::new();
    let b = Root::new();
    manager(&a, "a", "100.64.0.1");
    let bm = manager(&b, "b", "100.64.0.2");
    let token = random(32).unwrap();
    peer(&a, &bm, &token, &random(32).unwrap());
    assert!(!serde_json::to_string(&view_at(&a.0, true).unwrap())
        .unwrap()
        .contains(&token));
}
#[test]
#[ignore = "read-only live network identity check requires connected Tailscale"]
fn direct_sync_live_tailscale_identity_and_interfaces() {
    let net = super::super::machine_setup::tailscale::network().unwrap();
    assert!(!net.own.id.is_empty());
    assert!(!net.own.ips.is_empty());
    for ip in &net.own.ips {
        let listener = TcpListener::bind(std::net::SocketAddr::new(*ip, 0)).unwrap();
        drop(listener);
        assert!(service::eligible(&net, *ip).is_none());
    }
    println!(
        "Verified Tailscale identity, {} local interfaces and {} visible devices",
        net.own.ips.len(),
        net.peers.len()
    );
}

#[test]
fn direct_sync_identity_change_while_reading_a_request_prevents_catalog_publication() {
    let a = Root::new();
    let b = Root::new();
    let am = manager(&a, "a", "100.64.0.1");
    let bm = manager(&b, "b", "100.64.0.2");
    let bn = net(&bm, &am);
    let token = random(32).unwrap();
    peer(&b, &am, &token, &random(32).unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let root = b.0.clone();
    let source = node(&am);
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        wire::handle_with(
            &root,
            wire::Connection::new(stream),
            &bn,
            &source,
            source.ips[0],
            || {
                if calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0 {
                    Err("Tailscale identity changed".into())
                } else {
                    Ok(())
                }
            },
        )
    });
    let mut c = wire::Connection::new(TcpStream::connect(addr).unwrap());
    let cn = random(32).unwrap();
    c.send(&wire::Request::Auth {
        from: am.id.clone(),
        nonce: cn.clone(),
    })
    .unwrap();
    let wire::Response::Auth { nonce: sn, .. } = c.receive().unwrap() else {
        panic!()
    };
    let doc = Document::default();
    c.send(&wire::Request::Exchange {
        proof: wire::sign(&token, "request", &[&am.id, &bm.id, &cn, &sn], Some(&doc)).unwrap(),
        document: doc,
    })
    .unwrap();
    assert!(c.receive::<wire::Response>().is_err());
    assert!(server.join().unwrap().is_err());
    assert!(b.catalog().ssh.is_empty());
}
#[test]
fn direct_sync_cancel_before_finalization_preserves_the_previous_sync_source() {
    let a = Root::new();
    let b = Root::new();
    let am = manager(&a, "a", "100.64.0.1");
    let bm = manager(&b, "b", "100.64.0.2");
    let bn = net(&bm, &am);
    let previous = serde_json::json!({"version":1,"source":{"kind":"ssh","target":"you@original"},"device":"11111111111111111111111111111111","document":Document::default(),"baseline":[],"last_success":null,"last_error":null});
    store::write(&b.0, "catalog-sync.json", &previous).unwrap();
    let ticket = ticket_for(&b, &am.node);
    let request = ticket.nonce.clone();
    rpc(
        &b,
        bn,
        node(&am),
        wire::Request::Begin {
            from: am.id.clone(),
            request: request.clone(),
            token: random(32).unwrap(),
            ticket,
        },
    );
    approve_at(&b.0, &request, true).unwrap();
    approve_at(&b.0, &request, false).unwrap();
    assert_eq!(
        store::read::<serde_json::Value>(&b.0.join("catalog-sync.json"))
            .unwrap()
            .unwrap(),
        previous
    );
    assert!(store::transaction(&b.0, |s| Ok(s.peers.is_empty())).unwrap());
}
#[test]
fn direct_sync_live_network_parsing_pins_stable_ids_not_rotating_map_keys_or_non_tailnet_addresses()
{
    let bytes=serde_json::to_vec(&serde_json::json!({"BackendState":"Running","Self":{"ID":"self-stable","HostName":"Desktop","Online":true,"TailscaleIPs":["100.64.0.1","fd7a:115c:a1e0::1","192.168.0.1"]},"Peer":{"rotating-node-public-key":{"ID":"peer-stable","HostName":"Laptop","Online":true,"TailscaleIPs":["100.64.0.2"]}}})).unwrap();
    let net = super::super::machine_setup::tailscale::parse_network(&bytes).unwrap();
    assert_eq!(net.own.id, "self-stable");
    assert_eq!(net.own.ips.len(), 2);
    assert_eq!(net.peers[0].id, "peer-stable");
    assert!(service::eligible(&net, "192.168.0.1".parse().unwrap()).is_none());
    assert!(service::eligible(&net, "100.64.0.2".parse().unwrap()).is_some());
}
