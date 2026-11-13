//! Cached, client-only device list. Rendering never executes Tailscale.
use super::*;
use crate::client::machine_setup::TailscaleDevice;

#[derive(Debug, Default)]
pub(in crate::client::shell) struct Browser {
    pub request: u64,
    pub revision: u64,
    pub loading: bool,
    pub devices: Vec<TailscaleDevice>,
    pub error: Option<String>,
    pub query: TextEditor,
    pub selected: std::collections::BTreeMap<String, TailscaleDevice>,
}
impl Browser {
    pub fn matching_indices(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        self.devices
            .iter()
            .enumerate()
            .filter(|(_, device)| {
                device.online == Some(true)
                    && (query.is_empty()
                        || device.name.to_lowercase().contains(&query)
                        || device
                            .dns
                            .as_ref()
                            .is_some_and(|dns| dns.to_lowercase().contains(&query))
                        || device.ips.iter().any(|ip| ip.to_string().contains(&query)))
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn toggle(&mut self, index: usize, revision: u64) -> Result<(), String> {
        if self.loading || revision != self.revision || !self.matching_indices().contains(&index) {
            return Ok(());
        }
        let Some(device) = self.devices.get(index) else {
            return Ok(());
        };
        let key = device_key(device).ok_or("Device has no usable identity")?;
        if self.selected.remove(&key).is_none() {
            if self.selected.len() >= super::batch::LIMIT {
                return Err(format!(
                    "Select at most {} servers per batch.",
                    super::batch::LIMIT
                ));
            }
            self.selected.insert(key, device.clone());
        }
        Ok(())
    }
    pub fn refresh(&mut self, id: u64) -> Option<ClientShellAction> {
        if self.loading {
            return None;
        }
        self.request = machine_setup::next_id();
        self.loading = true;
        self.error = None;
        Some(ClientShellAction::MachineManager(Command::Tailscale {
            id,
            request: self.request,
        }))
    }
}

pub(super) fn device_key(device: &TailscaleDevice) -> Option<String> {
    if let Some(id) = &device.id {
        return Some(format!("node:{id}"));
    }
    // Older status providers: require the entire locator snapshot to match.
    // A refresh may never silently transfer a selection to another device.
    let mut ips = device.ips.clone();
    ips.sort();
    device
        .target()
        .map(|_| format!("legacy:{:?}:{:?}:{ips:?}", device.name, device.dns))
}
pub(super) fn render(
    buffer: &mut Buffer,
    inner: Rect,
    manager: &Manager,
    palette: &Palette,
    hits: &mut Vec<(Rect, Hit)>,
) -> Option<crate::protocol::CursorState> {
    use super::super::render::put_text;
    use ratatui::widgets::{Paragraph, Widget, Wrap};
    let browser = &manager.tailscale;
    let normal = Style::default().fg(palette.text).bg(palette.panel_bg);
    let muted = normal.fg(palette.overlay0);
    put_text(
        buffer,
        inner.x,
        inner.y + 1,
        inner.width,
        if manager.quick_add {
            "Click/Space selects · Enter adds selected · Type searches · Tab edits user"
        } else {
            "Online devices only · Type to search · ↑/↓ select · Enter fills form"
        },
        muted,
    );
    let search_width = inner.width.saturating_sub(8);
    let (query, cursor_col) = browser.query.viewport(search_width.saturating_sub(1));
    put_text(
        buffer,
        inner.x,
        inner.y + 2,
        inner.width,
        &format!("Search: {query}"),
        normal.bg(palette.active_row_bg),
    );
    hits.push((
        Rect::new(inner.x, inner.y + 2, inner.width, 1),
        Hit::TailscaleSearch,
    ));
    if manager.quick_add {
        let username = manager.fields[5].as_str();
        put_text(
            buffer,
            inner.x,
            inner.y + 3,
            inner.width,
            &format!(
                "Username: {}",
                if username.is_empty() {
                    "(SSH config / local user)"
                } else {
                    username
                }
            ),
            if manager.focus == 5 {
                normal.bg(palette.active_row_bg)
            } else {
                normal
            },
        );
        hits.push((
            Rect::new(inner.x, inner.y + 3, inner.width, 1),
            Hit::Field(5),
        ));
        put_text(
            buffer,
            inner.x,
            inner.y + 4,
            inner.width,
            "SSH key or password · Rust + key setup handled here",
            muted,
        );
    }
    let matches = browser.matching_indices();
    let online = browser
        .devices
        .iter()
        .filter(|device| device.online == Some(true))
        .count();
    let summary = if browser.loading {
        "Refreshing online devices…".into()
    } else {
        if manager.quick_add {
            format!(
                "{} selected · {} matching · {} online",
                browser.selected.len(),
                matches.len(),
                online
            )
        } else {
            format!("{} matching · {} online", matches.len(), online)
        }
    };
    put_text(
        buffer,
        inner.x,
        inner.y + if manager.quick_add { 5 } else { 3 },
        inner.width,
        &summary,
        normal,
    );
    let mut top = inner.y + if manager.quick_add { 7 } else { 5 };
    let bottom = inner
        .bottom()
        .saturating_sub(if manager.quick_add { 4 } else { 3 });
    if let Some(error) = &browser.error {
        if browser.devices.is_empty() {
            Paragraph::new(error.as_str())
                .wrap(Wrap { trim: false })
                .style(normal.fg(palette.accent))
                .render(
                    Rect::new(inner.x, top, inner.width, bottom.saturating_sub(top)),
                    buffer,
                );
        } else {
            put_text(
                buffer,
                inner.x,
                top,
                inner.width,
                "Refresh failed; showing previous devices. Press Ctrl+R to retry.",
                normal.fg(palette.accent),
            );
            top = top.saturating_add(2);
        }
    } else if matches.is_empty() && !browser.loading {
        let message = if online == 0 {
            if manager.quick_add {
                "No online devices are visible. Refresh or choose Manual / advanced to enter an address."
            } else {
                "No online Tailscale devices are visible. Check that your servers are connected, then Refresh; or go Back to enter an address."
            }
        } else {
            "No online devices match your search. Edit the search or press Ctrl+U to clear it."
        };
        Paragraph::new(message)
            .wrap(Wrap { trim: false })
            .style(muted)
            .render(
                Rect::new(inner.x, top, inner.width, bottom.saturating_sub(top)),
                buffer,
            );
    }
    let capacity = usize::from(bottom.saturating_sub(top));
    let selected = manager.selected.min(matches.len().saturating_sub(1));
    let height =
        |device: &TailscaleDevice| 1 + device.ips.len() + usize::from(device.dns.is_some());
    let mut start = selected;
    let mut used = matches
        .get(selected)
        .map_or(0, |index| height(&browser.devices[*index]));
    while start > 0 && used + height(&browser.devices[matches[start - 1]]) < capacity {
        start -= 1;
        used += height(&browser.devices[matches[start]]) + 1;
    }
    let mut y = top;
    for (position, index) in matches.iter().copied().enumerate().skip(start) {
        let device = &browser.devices[index];
        if y >= bottom {
            break;
        }
        let style = if manager.selected == position {
            normal.bg(palette.active_row_bg)
        } else {
            normal
        };
        let rows = (height(device) as u16).min(bottom - y);
        let rect = Rect::new(inner.x, y, inner.width, rows);
        buffer.set_style(rect, style);
        let status = "online";
        let status_width = status.len() as u16;
        let prefix = if manager.quick_add { 4 } else { 0 };
        if manager.quick_add {
            let checked = device_key(device).is_some_and(|key| browser.selected.contains_key(&key));
            put_text(
                buffer,
                inner.x,
                y,
                3,
                if checked { "[x]" } else { "[ ]" },
                style,
            );
        }
        put_text(
            buffer,
            inner.x + prefix,
            y,
            inner.width.saturating_sub(status_width + 1 + prefix),
            &device.name,
            style,
        );
        put_text(
            buffer,
            inner.right().saturating_sub(status_width),
            y,
            status_width,
            status,
            style.fg(palette.green),
        );
        let mut detail = y + 1;
        for ip in &device.ips {
            if detail >= y + rows {
                break;
            }
            put_text(
                buffer,
                inner.x + 1,
                detail,
                inner.width.saturating_sub(1),
                &ip.to_string(),
                style,
            );
            detail += 1;
        }
        if let Some(dns) = &device.dns {
            if detail < y + rows {
                put_text(
                    buffer,
                    inner.x + 1,
                    detail,
                    inner.width.saturating_sub(1),
                    dns,
                    style.fg(palette.overlay0),
                );
            }
        }
        hits.push((
            rect,
            if manager.quick_add {
                Hit::ToggleDevice(index, browser.revision)
            } else {
                Hit::TailscaleDevice(index)
            },
        ));
        y = y.saturating_add(rows + 1);
    }
    if manager.quick_add {
        let action = Rect::new(inner.x, inner.bottom() - 3, inner.width.min(23), 1);
        put_text(
            buffer,
            action.x,
            action.y,
            action.width,
            &format!("[ Add selected ({}) ]", browser.selected.len()),
            if browser.selected.is_empty() || browser.loading {
                muted
            } else {
                normal.fg(palette.accent)
            },
        );
        hits.push((action, Hit::AddSelected));
        if !browser.selected.is_empty() && inner.width >= 34 {
            let clear = Rect::new(inner.x + 24, inner.bottom() - 3, inner.width - 24, 1);
            put_text(
                buffer,
                clear.x,
                clear.y,
                clear.width,
                "[ Clear selection ]",
                normal,
            );
            hits.push((clear, Hit::ClearSelected));
        }
    }
    let refresh = Rect::new(
        inner.x,
        inner.bottom() - 2,
        inner.width.min(if manager.quick_add { 12 } else { 24 }),
        1,
    );
    put_text(
        buffer,
        refresh.x,
        refresh.y,
        refresh.width,
        if manager.quick_add {
            if browser.loading {
                "[ Loading ]"
            } else if manager.selected == matches.len() {
                "> Refresh"
            } else {
                "[ Refresh ]"
            }
        } else if browser.loading {
            "[ Refreshing… ]"
        } else if manager.selected == matches.len() {
            "> Refresh (Ctrl+R)"
        } else {
            "[ Refresh (Ctrl+R) ]"
        },
        if browser.loading {
            muted
        } else {
            normal.fg(palette.accent)
        },
    );
    hits.push((refresh, Hit::RefreshTailscale));
    if manager.quick_add && inner.width >= 24 {
        let manual = Rect::new(inner.x + 13, inner.bottom() - 2, inner.width - 13, 1);
        put_text(
            buffer,
            manual.x,
            manual.y,
            manual.width,
            if manual.width >= 21 {
                "[ Manual / advanced ]"
            } else {
                "[ Manual ]"
            },
            normal.fg(palette.accent),
        );
        hits.push((manual, Hit::ManualEntry));
    }
    (search_width > 0 && !(manager.quick_add && manager.focus == 5)).then_some(
        crate::protocol::CursorState {
            x: inner.x + 8 + cursor_col.min(search_width.saturating_sub(1)),
            y: inner.y + 2,
            visible: true,
            shape: 6,
        },
    )
}

pub(super) fn suggested_username(profiles: &[SavedSshEndpoint]) -> String {
    let users: std::collections::BTreeSet<_> = profiles
        .iter()
        .filter(|p| p.enabled)
        .filter_map(|p| {
            p.target
                .strip_prefix("ssh://")
                .unwrap_or(&p.target)
                .rsplit_once('@')
                .map(|(user, _)| user)
        })
        .collect();
    if users.len() == 1 {
        users.iter().next().copied().unwrap_or("").into()
    } else {
        String::new()
    }
}
pub(super) fn existing_profiles<'a>(
    profiles: &'a [SavedSshEndpoint],
    device: &TailscaleDevice,
    username: &str,
) -> Vec<&'a SavedSshEndpoint> {
    profiles
        .iter()
        .filter(|p| {
            let authority = p.target.strip_prefix("ssh://").unwrap_or(&p.target);
            let (user, host) = authority
                .rsplit_once('@')
                .map_or(("", authority), |(user, host)| (user, host));
            if user != username {
                return false;
            }
            let host = if let Some(rest) = host.strip_prefix('[') {
                rest.split_once(']').map_or(rest, |(host, _)| host)
            } else if host.matches(':').count() == 1 {
                host.split_once(':').map_or(host, |(host, _)| host)
            } else {
                host
            };
            device.dns.as_ref().is_some_and(|dns| {
                dns.trim_end_matches('.')
                    .eq_ignore_ascii_case(host.trim_end_matches('.'))
            }) || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| device.ips.contains(&ip))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_profile_matching_honors_username_and_dual_stack_addresses() {
        let profiles = vec![
            SavedSshEndpoint::new("Alice", "ssh://alice@[fd7a:115c:a1e0::2]:2222", "main").unwrap(),
            SavedSshEndpoint::new("Bob", "bob@build.tailnet.ts.net", "main").unwrap(),
        ];
        let device = TailscaleDevice {
            id: None,
            name: "Build".into(),
            dns: Some("build.tailnet.ts.net".into()),
            ips: vec![
                "100.90.0.2".parse().unwrap(),
                "fd7a:115c:a1e0::2".parse().unwrap(),
            ],
            online: Some(true),
        };
        assert_eq!(
            existing_profiles(&profiles, &device, "alice")[0].id,
            profiles[0].id
        );
        assert_eq!(
            existing_profiles(&profiles, &device, "bob")[0].id,
            profiles[1].id
        );
        assert!(existing_profiles(&profiles, &device, "").is_empty());
        assert!(suggested_username(&profiles).is_empty());
        let mut profiles = profiles;
        profiles[0].enabled = false;
        assert_eq!(suggested_username(&profiles), "bob");
    }
}
