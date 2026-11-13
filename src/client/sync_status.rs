//! UI presentation is built off-thread from bounded local observations.
use super::endpoint::catalog_sync::{
    monitor::{Report, Snapshot},
    Source,
};
use std::{collections::BTreeMap, io::Read, net::IpAddr, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Good,
    Waiting,
    Warning,
    Off,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct View {
    pub badge: String,
    pub compact: String,
    pub level: Level,
    pub details: String,
}
impl Default for View {
    fn default() -> Self {
        Self {
            badge: "Sync: checking…".into(),
            compact: "⇄".into(),
            level: Level::Waiting,
            details: "Checking manager sync status…".into(),
        }
    }
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn view() -> View {
    let mut view = match super::endpoint::catalog_sync::monitor::snapshot() {
        Ok(snapshot) => present(
            snapshot,
            names(&super::sync_background::root()),
            now(),
            super::sync_background::status()["running"] == true,
        ),
        Err(error) => View {
            badge: "Sync: error".into(),
            compact: "!".into(),
            level: Level::Warning,
            details: format!("Sync status unavailable: {error}"),
        },
    };
    view.details
        .push_str(&format!("\n{}", crate::private_updates::status_line()));
    view
}
pub(crate) fn remember_network(root: &Path, network: &super::machine_setup::tailscale::Network) {
    let mut names = BTreeMap::new();
    for node in std::iter::once(&network.own)
        .chain(network.peers.iter())
        .take(257)
    {
        let name: String = node
            .name
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        for ip in node.ips.iter().take(8) {
            names.insert(*ip, name.clone());
        }
    }
    let Ok(bytes) = serde_json::to_vec(&names) else {
        return;
    };
    if bytes.len() > 512 * 1024 {
        return;
    }
    let path = root.join("manager-device-names.json");
    if std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
        return;
    }
    let _ = super::endpoint::store_private_json(&path, &bytes, "manager device names");
}
fn names(root: &Path) -> BTreeMap<IpAddr, String> {
    let Ok(file) = std::fs::File::open(root.join("manager-device-names.json")) else {
        return BTreeMap::new();
    };
    let mut bytes = Vec::new();
    if file.take(512 * 1024 + 1).read_to_end(&mut bytes).is_err() || bytes.len() > 512 * 1024 {
        return BTreeMap::new();
    }
    let parsed: BTreeMap<IpAddr, String> = serde_json::from_slice(&bytes).unwrap_or_default();
    parsed
        .into_iter()
        .take(2056)
        .map(|(ip, name)| {
            (
                ip,
                name.chars().filter(|c| !c.is_control()).take(64).collect(),
            )
        })
        .collect()
}
fn age(report: &Report, exchange: u64, elapsed: u64) -> Option<u64> {
    if elapsed == u64::MAX {
        return None;
    }
    report
        .checked_at
        .checked_sub(exchange)
        .map(|age| age.saturating_add(elapsed))
}
fn ago(age: Option<u64>) -> String {
    match age {
        None => "unknown time".into(),
        Some(n) if n < 5 => "just now".into(),
        Some(n) if n < 60 => format!("{}s ago", n / 5 * 5),
        Some(n) if n < 3600 => format!("{}m ago", n / 60),
        Some(n) => format!("{}h ago", n / 3600),
    }
}
fn present(
    snapshot: Snapshot,
    names: BTreeMap<IpAddr, String>,
    now: u64,
    background: bool,
) -> View {
    let Some(source) = snapshot.source.as_ref() else {
        return View {
            badge: if snapshot.automatic {
                "Sync: checking…"
            } else {
                "Sync: off"
            }
            .into(),
            compact: "⇄".into(),
            level: if snapshot.automatic {
                Level::Waiting
            } else {
                Level::Off
            },
            details: if snapshot.automatic {
                "Automatic sync is waiting for an authenticated shared-list host."
            } else {
                "Server-list sync is off."
            }
            .into(),
        };
    };
    if *source == Source::Direct {
        return View {badge:"Sync: Direct".into(),compact:"⇄".into(),level:Level::Waiting,details:"Direct Tailscale pairing is selected. Choose “Pair another manager over Tailscale” for pairing status. Restricted SSH keys apply to shared-host sync.".into()};
    }
    let mut lines = vec![match source {
        Source::Ssh { target } => format!("Shared list via {target}"),
        _ => "Shared list hosted on this computer".into(),
    }];
    lines.push(
        if background {
            "Background sync active"
        } else {
            "Sync active while this UI is open"
        }
        .into(),
    );
    if let Some(error) = &snapshot.error {
        lines.push(format!("Attention: {error}"));
    }
    if snapshot.pending {
        lines.push("Local changes are waiting to sync.".into());
    }
    let Some(report) = snapshot.report.as_ref() else {
        lines.push("Waiting for manager status. Refresh status to check now.".into());
        return View {
            badge: if snapshot.error.is_some() {
                "Sync: unavailable"
            } else {
                "Sync: checking…"
            }
            .into(),
            compact: "⇄".into(),
            level: if snapshot.error.is_some() {
                Level::Warning
            } else {
                Level::Waiting
            },
            details: lines.join("\n"),
        };
    };
    let elapsed = now.checked_sub(snapshot.fetched_at);
    let fresh = elapsed.is_some_and(|n| n <= 45);
    let elapsed = elapsed.unwrap_or(u64::MAX);
    let matches = snapshot.local_digest.as_deref() == Some(report.catalog_digest.as_str());
    let usable = fresh && matches && !snapshot.pending && snapshot.error.is_none();
    if !fresh {
        lines.push("Manager status is stale; waiting for a fresh host check.".into());
    } else if !matches {
        lines.push("The shared host has a different catalog; synchronization is pending.".into());
    }
    lines.push(format!(
        "Host checked {} · updates automatically",
        ago(now.checked_sub(snapshot.fetched_at))
    ));
    let mut keys: BTreeMap<&str, Vec<_>> = BTreeMap::new();
    let mut legacy = Vec::new();
    let mut unknown = false;
    for row in &report.managers {
        if row.authentication == "restricted_key" {
            if let Some(fp) = row.key_fingerprint.as_deref() {
                keys.entry(fp).or_default().push(row);
            }
        } else if age(report, row.last_exchange, elapsed).is_some_and(|n| n <= 60) {
            if row.authentication == "legacy_ssh" {
                legacy.push(row.address);
            } else {
                unknown = true;
            }
        }
    }
    let mut current = 0;
    let mut self_current = false;
    for (fp, rows) in keys.iter_mut() {
        rows.sort_by_key(|row| std::cmp::Reverse(row.last_exchange));
        let latest = rows[0];
        let seen = age(report, latest.last_exchange, elapsed);
        let synced = usable && latest.in_sync && seen.is_some_and(|n| n <= 60);
        if synced {
            current += 1;
        }
        let this = snapshot.own_fingerprint.as_deref() == Some(*fp);
        if this && synced {
            self_current = true;
        }
        let label = names
            .get(&latest.address)
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| "Manager".into());
        lines.push(String::new());
        lines.push(format!(
            "{}{} · {}",
            label,
            if this { " (this Mac)" } else { "" },
            latest.address
        ));
        let state = if synced {
            "Current"
        } else if seen.is_none_or(|n| n > 60) {
            "Not seen recently"
        } else {
            "Not yet confirmed current"
        };
        lines.push(format!("  {state} · Restricted SSH key · {}", ago(seen)));
        lines.push(format!("  {fp}"));
    }
    legacy.sort();
    legacy.dedup();
    if !legacy.is_empty() {
        lines.push(String::new());
        lines.push("WARNING: older UI(s) still syncing with login keys:".into());
        for ip in &legacy {
            lines.push(format!("  {ip} · Legacy SSH · restart its older Herdr UI"));
        }
    } else {
        lines.push("No active legacy login-key sync observed.".into());
    }
    if unknown {
        lines.push("An active manager has an unrecognized credential mode.".into());
    }
    let needs_self = matches!(source, Source::Ssh { .. });
    if needs_self && !self_current {
        lines.push("This Mac's restricted-key sync is not yet confirmed by the host.".into());
    }
    let good = usable
        && keys.len() >= 2
        && current == keys.len()
        && legacy.is_empty()
        && !unknown
        && (!needs_self || self_current);
    let badge = if !legacy.is_empty() {
        "Sync: legacy key active".into()
    } else if unknown {
        "Sync: unknown key".into()
    } else if snapshot.error.is_some() || !fresh {
        "Sync: unavailable".into()
    } else if snapshot.pending {
        "Sync: changes pending".into()
    } else if keys.is_empty() {
        "Sync: awaiting peers".into()
    } else if usable && keys.len() == 1 && current == 1 {
        lines.push("Waiting for another manager to confirm its restricted-key sync.".into());
        "Sync: 1 current · waiting".into()
    } else if good {
        format!("Sync: {current} current · keys")
    } else {
        format!("Sync: {current}/{} current", keys.len())
    };
    View {
        badge,
        compact: if good { "✓" } else { "⇄!" }.into(),
        level: if good { Level::Good } else { Level::Warning },
        details: lines.join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::endpoint::catalog_sync::monitor::Manager;
    use super::*;
    fn snapshot() -> Snapshot {
        let fp = format!("SHA256:{}", "a".repeat(43));
        Snapshot {
            source: Some(Source::Ssh {
                target: "redie".into(),
            }),
            automatic: true,
            own_fingerprint: Some(fp.clone()),
            local_digest: Some("a".repeat(64)),
            pending: false,
            error: None,
            fetched_at: 1000,
            report: Some(Report {
                checked_at: 1000,
                catalog_digest: "a".repeat(64),
                managers: vec![
                    Manager {
                        address: "100.1.1.1".parse().unwrap(),
                        last_exchange: 995,
                        in_sync: true,
                        authentication: "restricted_key".into(),
                        key_fingerprint: Some(fp),
                    },
                    Manager {
                        address: "100.1.1.2".parse().unwrap(),
                        last_exchange: 995,
                        in_sync: true,
                        authentication: "restricted_key".into(),
                        key_fingerprint: Some(format!("SHA256:{}", "b".repeat(43))),
                    },
                ],
            }),
        }
    }
    #[test]
    fn fresh_distinct_restricted_keys_confirm_both_managers() {
        let v = present(snapshot(), BTreeMap::new(), 1005, true);
        assert_eq!(v.level, Level::Good);
        assert_eq!(v.badge, "Sync: 2 current · keys");
        assert!(v.details.contains("(this Mac)"));
        assert!(v.details.contains("Restricted SSH key"));
    }
    #[test]
    fn stale_pending_error_digest_mismatch_or_missing_self_never_show_green() {
        for n in 0..8 {
            let mut s = snapshot();
            let mut now = 1005;
            match n {
                0 => now = 1046,
                1 => s.pending = true,
                2 => s.error = Some("offline".into()),
                3 => s.local_digest = Some("c".repeat(64)),
                4 => s.own_fingerprint = None,
                5 => s.report.as_mut().unwrap().managers[1].last_exchange = 900,
                6 => now = 999,
                _ => s.report.as_mut().unwrap().managers[1].last_exchange = 1001,
            }
            assert_ne!(
                present(s, BTreeMap::new(), now, true).level,
                Level::Good,
                "case {n}"
            );
        }
    }
    #[test]
    fn one_manager_never_confirms_both_computers() {
        let mut s = snapshot();
        s.report.as_mut().unwrap().managers.pop();
        let v = present(s, BTreeMap::new(), 1005, true);
        assert_ne!(v.level, Level::Good);
        assert!(v.badge.contains("waiting"));
    }
    #[test]
    fn duplicate_addresses_for_a_key_do_not_inflate_manager_count() {
        let mut s = snapshot();
        let r = s.report.as_mut().unwrap();
        let mut duplicate = r.managers[0].clone();
        duplicate.address = "100.1.1.3".parse().unwrap();
        r.managers.push(duplicate);
        assert_eq!(
            present(s, BTreeMap::new(), 1005, true).badge,
            "Sync: 2 current · keys"
        );
    }
    #[test]
    fn active_legacy_on_same_address_warns_but_historical_legacy_does_not() {
        for active in [true, false] {
            let mut s = snapshot();
            let r = s.report.as_mut().unwrap();
            let mut legacy = r.managers[0].clone();
            legacy.authentication = "legacy_ssh".into();
            legacy.key_fingerprint = None;
            if !active {
                legacy.last_exchange = 900;
            }
            r.managers.push(legacy);
            let v = present(s, BTreeMap::new(), 1005, true);
            assert_eq!(v.level == Level::Good, !active);
            assert_eq!(v.details.contains("WARNING: older UI"), active);
        }
    }
}
