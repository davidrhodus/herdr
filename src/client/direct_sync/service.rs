use super::*;
use std::{
    fs::File,
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
type Context = Arc<Mutex<(Instant, Result<Network, String>)>>;
fn network(context: &Context) -> Result<Network, String> {
    let snapshot = context.lock().map_err(|_| "Manager discovery stopped")?;
    if snapshot.0.elapsed() > Duration::from_secs(20) {
        return Err("Tailscale status is stale; waiting to refresh".into());
    }
    snapshot.1.clone()
}
fn set_error(root: &Path, error: String) {
    let _ = store::transaction(root, |s| {
        s.error = Some(error);
        Ok(())
    });
}
pub(super) fn eligible(network: &Network, ip: IpAddr) -> Option<&Node> {
    network.peers.iter().find(|node| node.ips.contains(&ip))
}
fn begin_pair(root: &Path, manager: Manager, net: &Network) -> Result<(), String> {
    manager.validate()?;
    let node = net
        .peers
        .iter()
        .find(|node| node.id == manager.node && node.online && node.ips.contains(&manager.ip))
        .ok_or("Manager is no longer visible on Tailscale. Refresh and retry.")?;
    let (remote_id, ticket) = wire::probe_details(manager.ip)?;
    if remote_id != manager.id {
        return Err("Manager identity changed. Refresh and retry.".into());
    }
    store::transaction(root, |s| {
        identity_matches(s, net)?;
        if s.id == manager.id
            || s.peers
                .iter()
                .any(|p| p.manager.id == manager.id && p.manager.node != manager.node)
        {
            return Err("Manager identity conflicts with an existing pairing".into());
        }
        if s.outgoing
            .as_ref()
            .is_some_and(|p| p.expires > now() || p.confirmed)
        {
            return Err("Finish or cancel the current pairing first".into());
        }
        if s.peers.len() >= store::MAX_PEERS {
            return Err("At most 16 managers can be paired".into());
        }
        if s.incoming.iter().any(|p| {
            p.manager.id == manager.id
                && p.manager.node == manager.node
                && p.accepted != Some(false)
                && p.expires > now()
        }) {
            if s.id > manager.id {
                return Err(
                    "An incoming pairing request is already waiting. Approve it instead.".into(),
                );
            }
            for p in &mut s.incoming {
                if p.manager.id == manager.id {
                    p.accepted = Some(false);
                }
            }
        }
        s.error = None;
        s.outgoing = Some(store::Pending {
            request: ticket.nonce.clone(),
            manager: Manager {
                name: node.name.clone(),
                ..manager
            },
            token: random(32)?,
            expires: now() + store::TTL,
            accepted: None,
            epoch: None,
            confirmed: false,
            ticket,
            promotion: random(16)?,
        });
        Ok(())
    })
}
fn outgoing(root: &Path, net: &Network) -> Result<(), String> {
    let (own, pending) = store::transaction(root, |s| {
        identity_matches(s, net)?;
        Ok((s.id.clone(), s.outgoing.clone()))
    })?;
    let Some(pending) = pending else {
        return Ok(());
    };
    if pending.expires <= now() && !pending.confirmed {
        return store::transaction(root, |s| {
            if s.outgoing
                .as_ref()
                .is_some_and(|p| p.request == pending.request)
            {
                s.outgoing = None;
                s.error = Some("Pair request expired. Start pairing again.".into());
            }
            Ok(())
        });
    }
    if pending.confirmed {
        let result = wire::finalize_pending(root, net, &pending);
        if let Err(error) = &result {
            if error.starts_with("Pairing approval expired")
                || error.starts_with("Sync configuration changed")
            {
                let _ = cancel(root, &pending.request, Some(net));
                set_error(root, error.clone());
                return Ok(());
            }
        }
        return result;
    }
    if pending.accepted == Some(true) {
        return Ok(());
    }
    let node = net
        .peers
        .iter()
        .find(|n| n.id == pending.manager.node && n.online)
        .ok_or("Other manager is offline")?;
    let ip = node
        .ips
        .iter()
        .copied()
        .find(|ip| wire::probe(*ip).ok().as_deref() == Some(&pending.manager.id))
        .ok_or("Waiting for the other manager. Keep both UIs open.")?;
    let mut c = wire::Connection::connect(ip)?;
    c.send(&wire::Request::Begin {
        from: own.clone(),
        request: pending.request.clone(),
        token: pending.token.clone(),
        ticket: pending.ticket.clone(),
    })?;
    let wire::Response::Pair { phase, proof } = c.receive()? else {
        return Err("Other manager rejected pairing".into());
    };
    wire::verify(
        &pending.token,
        &phase,
        &[&own, &pending.manager.id, &pending.request],
        None,
        &proof,
    )?;
    store::transaction(root, |s| {
        identity_matches(s, net)?;
        let current = s
            .outgoing
            .as_mut()
            .filter(|p| p.request == pending.request && p.expires > now())
            .ok_or("Pair request changed or expired")?;
        match phase.as_str() {
            "accepted" => {
                current.accepted = Some(true);
                s.error = None;
            }
            "declined" => {
                s.outgoing = None;
                s.error = Some("Other manager declined pairing".into());
            }
            "pending" => {
                s.error = None;
            }
            _ => return Err("Invalid pairing reply".into()),
        }
        Ok(())
    })
}
fn discover(root: &Path, net: &Network, quit: &Arc<AtomicBool>) -> Result<(), String> {
    let peers = net
        .peers
        .iter()
        .filter(|n| n.online)
        .cloned()
        .collect::<Vec<_>>();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let found = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..8 {
            let peers = &peers;
            let next = &next;
            let found = &found;
            scope.spawn(move || {
                while !quit.load(Ordering::Acquire) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(node) = peers.get(i) else { break };
                    for ip in &node.ips {
                        if let Ok(id) = wire::probe(*ip) {
                            if let Ok(mut found) = found.lock() {
                                found.push(Manager {
                                    id,
                                    node: node.id.clone(),
                                    name: node.name.clone(),
                                    ip: *ip,
                                });
                            }
                            break;
                        }
                    }
                }
            });
        }
    });
    let mut managers = found.into_inner().map_err(|_| "Discovery stopped")?;
    managers.sort_by_key(|m| m.name.to_lowercase());
    store::transaction(root, |s| {
        identity_matches(s, net)?;
        s.error = if managers.is_empty() {
            Some(
                "No managers found. Open this Herdr build on the other computer, then refresh."
                    .into(),
            )
        } else {
            None
        };
        s.discovered = managers;
        s.scanning = false;
        Ok(())
    })
}
pub(super) fn confirm(root: &Path, request: &str) -> Result<(), String> {
    store::transaction(root, |s| {
        let p = s
            .outgoing
            .as_ref()
            .filter(|p| p.request == request && p.expires > now() && p.accepted == Some(true))
            .ok_or("Pairing has not been approved by the other manager, or expired")?;
        if s.peers.len() >= store::MAX_PEERS
            && !s.peers.iter().any(|peer| peer.manager.id == p.manager.id)
        {
            return Err("At most 16 managers can be paired".into());
        }
        let epoch = catalog_sync::direct_revision_at(root)?;
        if let Some(p) = s.outgoing.as_mut() {
            p.confirmed = true;
            p.epoch = Some(epoch);
        }
        s.error = None;
        Ok(())
    })
}

fn cancel(root: &Path, request: &str, net: Option<&Network>) -> Result<(), String> {
    let previous = store::transaction(root, |s| {
        let previous = s.outgoing.take().filter(|p| p.request == request);
        if previous.is_none() {
            return Err("Pairing request changed".into());
        }
        Ok((s.id.clone(), previous))
    })?;
    if let (own, Some(p)) = previous {
        if let Some(node) = net.and_then(|net| net.peers.iter().find(|n| n.id == p.manager.node)) {
            for ip in &node.ips {
                if let Ok(mut c) = wire::Connection::connect(*ip) {
                    let _ = c.send(&wire::Request::Cancel {
                        from: own.clone(),
                        request: p.request.clone(),
                        proof: wire::sign(
                            &p.token,
                            "cancel",
                            &[&own, &p.manager.id, &p.request],
                            None,
                        )?,
                    });
                    break;
                }
            }
        }
    }
    Ok(())
}
pub(super) fn forget(root: &Path, id: &str) -> Result<(), String> {
    store::transaction(root, |s| {
        s.peers.retain(|p| p.manager.id != id);
        s.incoming.retain(|p| p.manager.id != id);
        Ok(())
    })
}

fn sync(root: &Path, net: &Network, quit: &Arc<AtomicBool>) -> Result<(), String> {
    let peers = store::transaction(root, |s| {
        identity_matches(s, net)?;
        Ok(s.peers.clone())
    })?;
    for peer in peers {
        if quit.load(Ordering::Acquire) {
            break;
        }
        if let Err(error) = wire::sync_peer(root, net, &peer) {
            let _ = store::transaction(root, |s| {
                if let Some(current) = s
                    .peers
                    .iter_mut()
                    .find(|p| p.manager.id == peer.manager.id && p.pairing == peer.pairing)
                {
                    current.error = Some(error);
                }
                Ok(())
            });
        }
    }
    Ok(())
}
struct Owner {
    _lease: File,
    listeners: Vec<TcpListener>,
    addresses: Vec<IpAddr>,
    context: Context,
    jobs: Vec<thread::JoinHandle<()>>,
    net_due: Instant,
    sync_due: Instant,
    poll_due: Instant,
    net_busy: Arc<AtomicBool>,
    sync_busy: Arc<AtomicBool>,
    poll_busy: Arc<AtomicBool>,
    quit: Arc<AtomicBool>,
    error: Option<String>,
    admissions: Vec<(IpAddr, Instant)>,
}
impl Owner {
    fn new(lease: File) -> Self {
        Self {
            _lease: lease,
            listeners: vec![],
            addresses: vec![],
            context: Arc::new(Mutex::new((
                Instant::now(),
                Err("Checking Tailscale…".into()),
            ))),
            jobs: vec![],
            net_due: Instant::now(),
            sync_due: Instant::now(),
            poll_due: Instant::now(),
            net_busy: Arc::new(AtomicBool::new(false)),
            sync_busy: Arc::new(AtomicBool::new(false)),
            poll_busy: Arc::new(AtomicBool::new(false)),
            quit: Arc::new(AtomicBool::new(false)),
            error: None,
            admissions: vec![],
        }
    }
    fn spawn(
        &mut self,
        root: &Path,
        f: impl FnOnce(&Path, &Context, &Arc<AtomicBool>) -> Result<(), String> + Send + 'static,
    ) {
        let root = root.to_owned();
        let ctx = self.context.clone();
        let quit = self.quit.clone();
        self.jobs.push(thread::spawn(move || {
            if let Err(error) = f(&root, &ctx, &quit) {
                set_error(&root, error);
            }
        }));
    }
    fn tick(&mut self, root: &Path) {
        let mut remaining = Vec::new();
        for job in self.jobs.drain(..) {
            if job.is_finished() {
                let _ = job.join();
            } else {
                remaining.push(job);
            }
        }
        self.jobs = remaining;
        if Instant::now() >= self.net_due && !self.net_busy.swap(true, Ordering::AcqRel) {
            self.net_due = Instant::now() + Duration::from_secs(10);
            let busy = self.net_busy.clone();
            self.spawn(root, move |root, ctx, _| {
                let result = super::super::machine_setup::tailscale::network().and_then(|net| {
                    store::bind_identity(root, &net.own.id)?;
                    Ok(net)
                });
                if let Ok(mut current) = ctx.lock() {
                    *current = (Instant::now(), result);
                }
                busy.store(false, Ordering::Release);
                Ok(())
            });
        }
        match network(&self.context) {
            Ok(net) => {
                if net.own.ips != self.addresses || self.listeners.is_empty() {
                    self.listeners.clear();
                    self.addresses = net.own.ips.clone();
                    for ip in &self.addresses {
                        if let Ok(listener) =
                            TcpListener::bind(std::net::SocketAddr::new(*ip, PORT))
                        {
                            if listener.set_nonblocking(true).is_ok() {
                                self.listeners.push(listener);
                            }
                        }
                    }
                    self.error = if self.listeners.is_empty() {
                        Some("Cannot listen for managers on Tailscale. Another user or application may be using Herdr's sync port.".into())
                    } else {
                        None
                    };
                }
            }
            Err(error) => {
                self.listeners.clear();
                self.addresses.clear();
                self.error = Some(error);
            }
        }
        let mut accepted = Vec::new();
        for listener in &self.listeners {
            for _ in 0..8 {
                match listener.accept() {
                    Ok(pair) => accepted.push(pair),
                    Err(_) => break,
                }
            }
        }
        for (stream, address) in accepted {
            self.admissions
                .retain(|(_, at)| at.elapsed() < Duration::from_secs(10));
            let eligible_cached =
                network(&self.context).is_ok_and(|net| eligible(&net, address.ip()).is_some());
            if !eligible_cached
                || self.admissions.len() >= 64
                || self
                    .admissions
                    .iter()
                    .filter(|(ip, _)| *ip == address.ip())
                    .count()
                    >= 24
            {
                drop(stream);
                continue;
            }
            self.admissions.push((address.ip(), Instant::now()));
            if self.jobs.len() >= 16 {
                drop(stream);
                continue;
            }
            self.spawn(root, move |root, context, _| {
                let net = network(context)?;
                let source = eligible(&net, address.ip())
                    .ok_or("Connection is not from a visible Tailscale device")?;
                let _ = wire::handle(
                    root,
                    wire::Connection::new(stream),
                    &net,
                    source,
                    address.ip(),
                );
                Ok(())
            });
        }
        let queue = root.join("direct-sync-commands");
        let entries = std::fs::read_dir(&queue)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .take(16)
            .collect::<Vec<_>>();
        for entry in entries {
            if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let action = store::read::<Action>(&entry.path());
            let _ = std::fs::remove_file(entry.path());
            if let Ok(Some(action)) = action {
                if self.jobs.len() >= 16
                    && matches!(
                        action,
                        Action::Discover
                            | Action::Pair { .. }
                            | Action::Approve { .. }
                            | Action::Confirm { .. }
                    )
                {
                    set_error(root, "Manager sync is busy. Retry in a moment.".into());
                    continue;
                }
                match action {
                    Action::Discover => {
                        let start = store::transaction(root, |s| {
                            if s.scanning {
                                return Ok(false);
                            }
                            s.scanning = true;
                            s.error = None;
                            Ok(true)
                        })
                        .unwrap_or(false);
                        if !start {
                            continue;
                        }
                        self.spawn(root, |root, ctx, quit| {
                            let result = network(ctx).and_then(|net| discover(root, &net, quit));
                            if result.is_err() {
                                let _ = store::transaction(root, |s| {
                                    s.scanning = false;
                                    Ok(())
                                });
                            }
                            result
                        });
                    }
                    Action::Pair { manager } => {
                        if self.listeners.is_empty() {
                            set_error(
                                root,
                                "Tailscale manager sync is not listening yet. Retry in a moment."
                                    .into(),
                            );
                        } else {
                            self.spawn(root, move |root, ctx, _| {
                                let _ = ctx;
                                begin_pair(
                                    root,
                                    manager,
                                    &super::super::machine_setup::tailscale::network()?,
                                )
                            });
                        }
                    }
                    Action::Approve { request, accepted } => {
                        self.spawn(root, move |root, _, _| {
                            let net = super::super::machine_setup::tailscale::network()?;
                            store::transaction(root, |s| identity_matches(s, &net))?;
                            approve_at(root, &request, accepted)
                        });
                    }
                    Action::Confirm { request } => {
                        self.spawn(root, move |root, _, quit| {
                            let net = super::super::machine_setup::tailscale::network()?;
                            store::transaction(root, |s| identity_matches(s, &net))?;
                            confirm(root, &request)?;
                            outgoing(root, &net)?;
                            sync(root, &net, quit)
                        });
                    }
                    Action::Cancel { request } => self.spawn(root, move |root, ctx, _| {
                        cancel(root, &request, network(ctx).ok().as_ref())
                    }),
                    Action::Forget { id } => {
                        if let Err(error) = forget(root, &id) {
                            set_error(root, error);
                        }
                    }
                    Action::SyncNow => self.sync_due = Instant::now(),
                    Action::Disable => match super::disable_at(root) {
                        Ok(()) => self.error = None,
                        Err(error) => {
                            set_error(root, error.clone());
                            self.error = Some(error);
                        }
                    },
                }
            }
        }
        if Instant::now() >= self.sync_due && !self.sync_busy.swap(true, Ordering::AcqRel) {
            self.sync_due = Instant::now() + Duration::from_secs(15);
            let busy = self.sync_busy.clone();
            self.spawn(root, move |root, ctx, quit| {
                let result = network(ctx).map_or(Ok(()), |net| sync(root, &net, quit));
                busy.store(false, Ordering::Release);
                result
            });
        }
        if Instant::now() >= self.poll_due && !self.poll_busy.swap(true, Ordering::AcqRel) {
            self.poll_due = Instant::now() + Duration::from_secs(1);
            let busy = self.poll_busy.clone();
            self.spawn(root, move |root, ctx, _| {
                let result = network(ctx).map_or(Ok(()), |net| outgoing(root, &net));
                busy.store(false, Ordering::Release);
                result
            });
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Release);
        self.listeners.clear();
        for job in self.jobs.drain(..) {
            let _ = job.join();
        }
    }
}
pub(super) fn start(
    tx: Option<tokio::sync::mpsc::Sender<super::super::ClientLoopEvent>>,
    quit: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let root = store::root();
        let mut owner: Option<Owner> = None;
        let mut previous = None;
        let mut resident = false;
        let mut resident_due = Instant::now();
        while !quit.load(Ordering::Acquire) {
            if tx.is_some() && Instant::now() >= resident_due {
                resident_due = Instant::now() + Duration::from_secs(5);
                resident = super::super::sync_background::status()["running"] == true;
                if resident {
                    owner = None;
                }
            }
            if owner.is_none() && !resident {
                if let Ok(lease) = store::lock(&root, "direct-sync-owner.lock") {
                    if lease.try_lock().is_ok() {
                        let _ = store::transaction(&root, |s| {
                            s.scanning = false;
                            Ok(())
                        });
                        owner = Some(Owner::new(lease));
                    }
                }
            }
            let view = if let Some(owner) = owner.as_mut() {
                owner.tick(&root);
                match view_at(&root, !owner.listeners.is_empty()) {
                    Ok(mut view) => {
                        if owner.error.is_some() {
                            view.error = owner.error.clone();
                        }
                        Some(view)
                    }
                    Err(error) => Some(View {
                        error: Some(error),
                        ..View::default()
                    }),
                }
            } else {
                store::read::<View>(&root.join("direct-sync-view.json"))
                    .ok()
                    .flatten()
            };
            if let Some(view) = view {
                if previous.as_ref() != Some(&view) {
                    if owner.is_some() {
                        let _ = store::write(&root, "direct-sync-view.json", &view);
                    }
                    previous = Some(view.clone());
                    if let Some(tx) = &tx {
                        if tx
                            .blocking_send(super::super::ClientLoopEvent::DirectSync(view))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(if owner.is_some() {
                200
            } else {
                1000
            }));
        }
    });
}
