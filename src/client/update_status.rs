//! Local-only presentation of the elected updater's bounded host observations.
use super::sync_status::{Level, ManagerRow};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};
pub(super) struct View {
    pub badge: String,
    pub level: Level,
    pub details: String,
    pub managers: Vec<ManagerRow>,
}
fn clean(value: &Value, limit: usize) -> Option<String> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= limit && !s.chars().any(char::is_control))
        .map(str::to_owned)
}
fn last_seen(age: Option<u64>, host_fresh: bool) -> String {
    let Some(age) = age else {
        return "?".into();
    };
    let short = match age {
        0..60 => format!("{}s", age / 5 * 5),
        60..3600 => format!("{}m", age / 60),
        3600..86400 => format!("{}h", age / 3600),
        _ if age / 86400 <= 9999 => format!("{}d", age / 86400),
        _ => "9999d+".into(),
    };
    if !host_fresh {
        "host stale".into()
    } else if age > 150 {
        format!("{short} stale")
    } else {
        short
    }
}
pub(super) fn present(
    value: Result<Value, String>,
    names: &BTreeMap<IpAddr, String>,
    aliases: &BTreeMap<IpAddr, IpAddr>,
    expected: BTreeSet<IpAddr>,
    now: u64,
) -> View {
    let unavailable = |reason: String| View {
        badge: "Updates: unconfirmed".into(),
        level: Level::Waiting,
        details: format!("Manager updates\n{reason}\nClick Refresh status to check again."),
        managers: expected
            .iter()
            .map(|ip| aliases.get(ip).copied().unwrap_or(*ip))
            .collect::<BTreeSet<_>>()
            .iter()
            .map(|ip| ManagerRow {
                name: names.get(ip).cloned().unwrap_or_else(|| ip.to_string()),
                status: "? Version unknown · auto ?".into(),
                compact: "unknown auto ?".into(),
                last_seen: "?".into(),
                level: Level::Waiting,
            })
            .collect(),
    };
    let value = match value {
        Ok(v) => v,
        Err(e) => return unavailable(e),
    };
    if let Some(error) = clean(&value["error"], 512) {
        return unavailable(error);
    }
    let Some(rows) = value["peers"]["managers"]
        .as_array()
        .filter(|r| r.len() <= 32)
    else {
        return unavailable("Waiting for manager update reports from the host.".into());
    };
    let elapsed = value["fetched_at"]
        .as_u64()
        .and_then(|t| now.checked_sub(t));
    let fresh = elapsed.is_some_and(|t| t <= 90);
    let published = clean(&value["peers"]["published_build"], 128);
    let canonical = |ip: IpAddr| aliases.get(&ip).copied().unwrap_or(ip);
    let mut managers: BTreeMap<IpAddr, &Value> = BTreeMap::new();
    for row in rows {
        if let Some(ip) = row["address"]
            .as_str()
            .and_then(|v| v.parse::<IpAddr>().ok())
        {
            let ip = canonical(ip);
            if managers.get(&ip).is_none_or(|old| {
                row["age_seconds"].as_u64().unwrap_or(u64::MAX)
                    < old["age_seconds"].as_u64().unwrap_or(u64::MAX)
            }) {
                managers.insert(ip, row);
            }
        }
    }
    let mut addresses: BTreeSet<_> = expected.into_iter().map(canonical).collect();
    addresses.extend(managers.keys().copied());
    let mut lines = vec!["Manager updates via Redie (background updater reports)".into()];
    if !fresh {
        lines.push("Host status is stale; versions below are last reported.".into());
    }
    let mut current = 0;
    let mut sidebar = Vec::new();
    for ip in &addresses {
        lines.push(String::new());
        lines.push(format!(
            "{} · {ip}",
            names
                .get(ip)
                .filter(|n| !n.is_empty())
                .map(String::as_str)
                .unwrap_or("Manager")
        ));
        let name = names
            .get(ip)
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| ip.to_string());
        let Some(row) = managers.get(ip) else {
            sidebar.push(ManagerRow {
                name,
                status: "? Version unknown · auto ?".into(),
                compact: "unknown auto ?".into(),
                last_seen: "?".into(),
                level: Level::Waiting,
            });
            lines.push("  Version unknown · no update report yet".into());
            continue;
        };
        let report = &row["client_reported"];
        let version = clean(&row["display_version"], 64);
        let age = row["age_seconds"]
            .as_u64()
            .zip(elapsed)
            .map(|(a, b)| a.saturating_add(b));
        let recent = fresh && age.is_some_and(|a| a <= 150);
        let automatic = report["enabled"] == true && report["automatic"] == true;
        let matches = published.as_deref().is_some_and(|id| {
            report["running_build"].as_str() == Some(id)
                && report["installed"]["build_id"].as_str() == Some(id)
        });
        let good = recent
            && automatic
            && version.is_some()
            && matches
            && row["current"] == true
            && report["error"].is_null();
        if good {
            current += 1;
        }
        let state = if !recent {
            "Not seen recently"
        } else if report["enabled"] != true {
            "Updates disabled"
        } else if !report["error"].is_null() {
            "Update error"
        } else if !automatic {
            "Automatic updates not confirmed"
        } else if !matches {
            "Update pending"
        } else if version.is_none() {
            "Version label unavailable; update this manager"
        } else if good {
            "Up to date"
        } else {
            "Update not yet confirmed"
        };
        let auto = if !recent {
            "?"
        } else if automatic {
            "on"
        } else if report["enabled"] == false {
            "off"
        } else {
            "?"
        };
        let short_version = version
            .as_deref()
            .and_then(|v| v.split_whitespace().last())
            .filter(|s| {
                s.starts_with('b')
                    && s.len() <= 11
                    && s.len() > 1
                    && s[1..].bytes().all(|b| b.is_ascii_digit())
            })
            .unwrap_or("unknown");
        sidebar.push(ManagerRow {
            name,
            status: format!(
                "{} {} · auto {auto}",
                if good {
                    "✓"
                } else if !recent {
                    "?"
                } else {
                    "!"
                },
                version.as_deref().unwrap_or("Version unknown")
            ),
            compact: format!("{short_version} auto {auto}"),
            last_seen: last_seen(age, fresh),
            level: if good {
                Level::Good
            } else if !recent {
                Level::Waiting
            } else {
                Level::Warning
            },
        });
        lines.push(format!(
            "  {} · {state}",
            version.unwrap_or_else(|| "Version unknown".into())
        ));
        lines.push(format!(
            "  Automatic updates: {} · last report: {}",
            if automatic {
                "on"
            } else if report["enabled"] == false {
                "off"
            } else {
                "unconfirmed"
            },
            age.map(|n| format!("{n}s ago"))
                .unwrap_or_else(|| "unknown".into())
        ));
        if let Some(error) = clean(&report["error"], 512) {
            lines.push(format!("  {error}"));
        }
    }
    let total = addresses.len();
    let good = fresh && total >= 2 && current == total;
    View {
        badge: if good {
            format!("Updates: {current} current · auto")
        } else {
            format!("Updates: {current}/{total} current")
        },
        level: if good { Level::Good } else { Level::Warning },
        details: lines.join("\n"),
        managers: sidebar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observation() -> Value {
        let rows: Vec<_> = ["100.1.1.1", "100.1.1.2"].into_iter().map(|ip| serde_json::json!({"address":ip,"age_seconds":5,"display_version":"v0.9.5 b2","current":true,"client_reported":{"enabled":true,"automatic":true,"running_build":"build","installed":{"build_id":"build"},"error":null}})).collect();
        serde_json::json!({"fetched_at":1000,"peers":{"published_build":"build","managers":rows}})
    }
    fn view(value: Value, at: u64) -> View {
        present(
            Ok(value),
            &BTreeMap::from([
                ("100.1.1.1".parse().unwrap(), "Laptop".into()),
                ("100.1.1.2".parse().unwrap(), "Desktop".into()),
            ]),
            &BTreeMap::new(),
            ["100.1.1.1".parse().unwrap(), "100.1.1.2".parse().unwrap()].into(),
            at,
        )
    }
    #[test]
    fn both_managers_show_versions_automatic_updates_and_current_state() {
        let v = view(observation(), 1005);
        assert_eq!(v.level, Level::Good);
        for expected in [
            "Laptop",
            "Desktop",
            "v0.9.5 b2",
            "Automatic updates: on",
            "Up to date",
        ] {
            assert!(v.details.contains(expected), "{expected}");
        }
        assert_eq!(v.badge, "Updates: 2 current · auto");
        assert_eq!(v.managers[0].name, "Laptop");
        assert_eq!(v.managers[1].name, "Desktop");
        assert_eq!(v.managers[0].status, "✓ v0.9.5 b2 · auto on");
    }
    #[test]
    fn stale_future_missing_old_disabled_failed_and_mismatched_reports_never_confirm_current() {
        for case in 0..11 {
            let mut v = observation();
            let mut at = 1005;
            match case {
                0 => at = 1091,
                1 => at = 999,
                2 => v["peers"]["managers"][1]["age_seconds"] = 151.into(),
                3 => {
                    v["peers"]["managers"].as_array_mut().unwrap().pop();
                }
                4 => v["peers"]["managers"][1]["display_version"] = Value::Null,
                5 => v["peers"]["managers"][1]["client_reported"]["automatic"] = false.into(),
                6 => v["peers"]["managers"][1]["client_reported"]["enabled"] = false.into(),
                7 => v["peers"]["managers"][1]["client_reported"]["error"] = "offline".into(),
                8 => v["peers"]["managers"][1]["client_reported"]["running_build"] = "older".into(),
                9 => v["peers"]["managers"][1]["age_seconds"] = Value::Null,
                _ => v["error"] = "Host unavailable".into(),
            }
            let view = view(v, at);
            assert_ne!(view.level, Level::Good, "case {case}");
            assert_ne!(view.managers[1].level, Level::Good, "case {case}");
            assert!(!view.managers[1].status.starts_with('✓'), "case {case}");
        }
    }
    #[test]
    fn dual_stack_addresses_of_one_tailscale_node_count_once() {
        let ipv4 = "100.1.1.1".parse().unwrap();
        let ipv6: IpAddr = "fd7a:115c:a1e0::1".parse().unwrap();
        let expected = [ipv6, "100.1.1.2".parse().unwrap()].into();
        let mut value = observation();
        let mut duplicate = value["peers"]["managers"][0].clone();
        duplicate["address"] = ipv6.to_string().into();
        value["peers"]["managers"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        let v = present(
            Ok(value),
            &BTreeMap::new(),
            &BTreeMap::from([(ipv6, ipv4)]),
            expected,
            1005,
        );
        assert_eq!(v.level, Level::Good);
        assert_eq!(v.badge, "Updates: 2 current · auto");
    }
    #[test]
    fn sidebar_checkins_age_locally_and_distinguish_stale_host_from_stale_manager() {
        let v = view(observation(), 1010);
        assert_eq!(v.managers[0].last_seen, "15s");
        let mut old = observation();
        old["peers"]["managers"][1]["age_seconds"] = 180.into();
        let v = view(old, 1005);
        assert_eq!(v.managers[1].last_seen, "3m stale");
        assert_ne!(v.managers[1].level, Level::Good);
        let v = view(observation(), 1091);
        assert_eq!(v.managers[0].last_seen, "host stale");
        let v = view(observation(), 999);
        assert_eq!(v.managers[0].last_seen, "?");
        let mut missing = observation();
        missing["peers"]["managers"].as_array_mut().unwrap().pop();
        assert_eq!(view(missing, 1005).managers[1].last_seen, "?");
        assert!(last_seen(Some(u64::MAX), true).len() <= 20);
    }
    #[test]
    fn control_characters_in_remote_versions_are_not_drawn() {
        let mut v = observation();
        v["peers"]["managers"][1]["display_version"] = "bad\u{1b}[2J".into();
        let v = view(v, 1005);
        assert_ne!(v.level, Level::Good);
        assert!(!v.details.contains('\u{1b}'));
    }
}
