//! Local Tailscale discovery only. No tailnet data crosses Herdr's wire codecs.
use std::{
    net::IpAddr,
    process::{Command, Stdio},
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Device {
    pub name: String,
    pub dns: Option<String>,
    pub ips: Vec<IpAddr>,
    pub online: Option<bool>,
}
impl Device {
    pub fn target(&self) -> Option<String> {
        self.dns
            .clone()
            .or_else(|| self.ips.first().map(ToString::to_string))
    }
}
fn dns_name(value: &str) -> Option<String> {
    let value = value.trim_end_matches('.');
    (!value.is_empty()
        && value.len() <= 253
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b)))
    .then(|| value.to_owned())
}
pub(super) fn parse(bytes: &[u8]) -> Result<Vec<Device>, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "Tailscale returned invalid status data")?;
    match value["BackendState"].as_str() {
        Some("Running") => {}
        Some("NeedsLogin") => {
            return Err(
                "Sign in to Tailscale, then Refresh. Manual entry is still available.".into(),
            )
        }
        Some("NeedsMachineAuth") => {
            return Err("Approve this computer in Tailscale, then Refresh.".into())
        }
        Some("Stopped") => {
            return Err("Tailscale is disconnected. Connect in Tailscale, then Refresh.".into())
        }
        Some("Starting" | "NoState") => {
            return Err("Tailscale is starting. Refresh in a moment.".into())
        }
        _ => {
            return Err(
                "Could not read Tailscale connection status. Open Tailscale and Refresh.".into(),
            )
        }
    }
    let peers = match &value["Peer"] {
        serde_json::Value::Null => return Ok(Vec::new()),
        serde_json::Value::Object(peers) => peers,
        _ => return Err("Tailscale returned invalid device data".into()),
    };
    let mut devices = peers
        .values()
        .filter_map(|peer| {
            let dns = peer["DNSName"].as_str().and_then(dns_name);
            let mut ips: Vec<IpAddr> = peer["TailscaleIPs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str()?.parse().ok())
                .take(8)
                .collect();
            ips.sort_by_key(|ip| (!ip.is_ipv4(), *ip));
            ips.dedup();
            if dns.is_none() && ips.is_empty() {
                return None;
            }
            let name = peer["HostName"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect::<String>();
            let name = if name.trim().is_empty() {
                dns.clone().unwrap_or_else(|| ips[0].to_string())
            } else {
                name
            };
            Some(Device {
                name,
                dns,
                ips,
                online: peer["Online"].as_bool(),
            })
        })
        .collect::<Vec<_>>();
    fn rank(value: Option<bool>) -> u8 {
        match value {
            Some(true) => 0,
            None => 1,
            Some(false) => 2,
        }
    }
    devices.sort_by_cached_key(|device| {
        (
            rank(device.online),
            device.name.to_lowercase(),
            device.target(),
        )
    });
    devices.truncate(256);
    Ok(devices)
}
fn command(executable: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(executable);
    command
        .args(["status", "--json"])
        .env("TAILSCALE_BE_CLI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}
pub(crate) fn status_bytes() -> Result<Vec<u8>, String> {
    let child = match command("tailscale").spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let path=crate::platform::tailscale_cli_fallback().ok_or("Tailscale was not found. Install or open Tailscale, then Refresh; or enter a host manually.")?;
            command(path)
                .spawn()
                .map_err(|_| "Could not start Tailscale. Open Tailscale, then Refresh.")?
        }
        Err(_) => {
            return Err("Could not start Tailscale. Check its installation, then Refresh.".into())
        }
    };
    let output = crate::remote::bounded_discovery_output(
        child,
        Duration::from_secs(3),
        Arc::new(AtomicBool::new(false)),
        1024 * 1024,
    )
    .map_err(|_| "Tailscale status failed or timed out. Open Tailscale, then Refresh.")?;
    if !output.status.success() {
        return Err(
            "Could not read Tailscale status. Open Tailscale and connect, then Refresh.".into(),
        );
    }
    Ok(output.stdout)
}
pub(super) fn devices() -> Result<Vec<Device>, String> {
    parse(&status_bytes()?)
}

#[derive(Clone)]
pub(crate) struct Node {
    pub id: String,
    pub name: String,
    pub ips: Vec<IpAddr>,
    pub online: bool,
}
#[derive(Clone)]
pub(crate) struct Network {
    pub own: Node,
    pub peers: Vec<Node>,
}
pub(crate) fn tailscale_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let b = ip.octets();
            b[0] == 100 && (64..=127).contains(&b[1])
        }
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}
pub(crate) fn parse_network(bytes: &[u8]) -> Result<Network, String> {
    parse(bytes)?;
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid Tailscale status")?;
    let node = |value: &serde_json::Value| -> Option<Node> {
        let id = value["ID"].as_str()?.to_owned();
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            return None;
        }
        let name = value["HostName"]
            .as_str()
            .filter(|name| !name.trim().is_empty())
            .or_else(|| {
                value["DNSName"]
                    .as_str()
                    .filter(|name| !name.trim().is_empty())
            })
            .unwrap_or("Herdr manager")
            .chars()
            .filter(|c| !c.is_control())
            .take(128)
            .collect();
        let ips = value["TailscaleIPs"]
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str()?.parse::<IpAddr>().ok())
            .filter(|ip| tailscale_ip(*ip))
            .take(8)
            .collect::<Vec<_>>();
        if ips.is_empty() {
            return None;
        }
        Some(Node {
            id,
            name,
            ips,
            online: value["Online"].as_bool() == Some(true),
        })
    };
    let own = node(&value["Self"]).ok_or("Tailscale has no local device identity or address")?;
    let peers = value["Peer"]
        .as_object()
        .into_iter()
        .flat_map(|p| p.values())
        .filter_map(node)
        .take(256)
        .collect();
    Ok(Network { own, peers })
}
pub(crate) fn network() -> Result<Network, String> {
    parse_network(&status_bytes()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sorts_online_first_preserves_both_ip_families_and_prefers_dns() {
        let devices=parse(&serde_json::to_vec(&serde_json::json!({"BackendState":"Running","Peer":{
            "a":{"HostName":"Offline","Online":false,"DNSName":"offline.tailnet.ts.net.","TailscaleIPs":["100.90.0.3"]},
            "b":{"HostName":"Build","Online":true,"DNSName":"build.tailnet.ts.net.","TailscaleIPs":["fd7a:115c:a1e0::1","invalid","100.90.0.2","100.90.0.2"]},
            "c":{"HostName":"Unnamed","TailscaleIPs":["100.90.0.4"]},
            "d":{"HostName":"invalid","DNSName":"bad@host","TailscaleIPs":["bad"]}
        }})).unwrap()).unwrap();
        assert_eq!(
            devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["Build", "Unnamed", "Offline"]
        );
        assert_eq!(devices[0].target().as_deref(), Some("build.tailnet.ts.net"));
        assert_eq!(
            devices[0]
                .ips
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["100.90.0.2", "fd7a:115c:a1e0::1"]
        );
        assert_eq!(devices[1].target().as_deref(), Some("100.90.0.4"));
        assert_eq!(devices[1].online, None);
    }
    #[test]
    fn disconnected_malformed_empty_and_missing_status_are_distinct() {
        assert!(parse(br#"{"BackendState":"Stopped"}"#)
            .unwrap_err()
            .contains("disconnected"));
        assert!(parse(br#"{"BackendState":"NeedsLogin"}"#)
            .unwrap_err()
            .contains("Sign in"));
        assert!(parse(br#"{"BackendState":"NeedsMachineAuth"}"#)
            .unwrap_err()
            .contains("Approve"));
        assert!(parse(b"not json").is_err());
        assert!(parse(b"{}").is_err());
        assert!(parse(br#"{"BackendState":"Running","Peer":[]}"#).is_err());
        assert!(parse(br#"{"BackendState":"Running","Peer":null}"#)
            .unwrap()
            .is_empty());
    }
    #[test]
    #[ignore = "read-only live check requires installed, connected Tailscale"]
    fn live_installed_tailscale_discovery() {
        let devices = devices().expect("read installed Tailscale status");
        assert!(
            !devices.is_empty(),
            "connected Tailscale has no visible peers"
        );
        println!(
            "Discovered {} devices, {} online",
            devices.len(),
            devices
                .iter()
                .filter(|device| device.online == Some(true))
                .count()
        );
    }

    #[test]
    fn gui_binary_is_forced_to_cli_mode_without_interactive_stdin() {
        let command = command("tailscale");
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "TAILSCALE_BE_CLI"
                && value == Some(std::ffi::OsStr::new("1"))));
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["status", "--json"]);
    }
}
