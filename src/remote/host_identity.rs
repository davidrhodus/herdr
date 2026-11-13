//! Public host locators shared by local setup planning and the fresh CLI guard.
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
pub(crate) const SETUP_HOST_ENV: &str = "HERDR_SETUP_HOST_IDENTITY";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostIdentity {
    pub dns: Option<String>,
    pub ips: Vec<IpAddr>,
}
impl HostIdentity {
    pub fn validate(&self) -> Result<(), String> {
        if self.ips.len() > 8
            || (self.ips.is_empty() && self.dns.is_none())
            || self.dns.as_ref().is_some_and(|s| {
                s.is_empty()
                    || s.len() > 253
                    || !s
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
            })
        {
            return Err("Invalid selected host identity".into());
        }
        Ok(())
    }
    pub fn matches_target(&self, target: &str) -> bool {
        let authority = target.strip_prefix("ssh://").unwrap_or(target);
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = if let Some(rest) = host.strip_prefix('[') {
            rest.split_once(']').map_or(rest, |(host, _)| host)
        } else if host.matches(':').count() == 1 {
            host.split_once(':').map_or(host, |(host, _)| host)
        } else {
            host
        };
        self.dns.as_ref().is_some_and(|dns| {
            dns.trim_end_matches('.')
                .eq_ignore_ascii_case(host.trim_end_matches('.'))
        }) || host
            .parse::<IpAddr>()
            .is_ok_and(|ip| self.ips.contains(&ip))
    }
}
