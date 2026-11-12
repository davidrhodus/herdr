//! Cached, client-only device list. Rendering never executes Tailscale.
use super::*;
use crate::client::machine_setup::TailscaleDevice;

#[derive(Debug, Default)]
pub(in crate::client::shell) struct Browser {
    pub request: u64,
    pub loading: bool,
    pub devices: Vec<TailscaleDevice>,
    pub error: Option<String>,
    pub query: TextEditor,
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
        "Online devices only · Type to search · ↑/↓ select · Enter fills form",
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
    let matches = browser.matching_indices();
    let online = browser
        .devices
        .iter()
        .filter(|device| device.online == Some(true))
        .count();
    let summary = if browser.loading {
        "Refreshing online devices…".into()
    } else {
        format!("{} matching · {} online", matches.len(), online)
    };
    put_text(buffer, inner.x, inner.y + 3, inner.width, &summary, normal);
    let mut top = inner.y + 5;
    let bottom = inner.bottom().saturating_sub(3);
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
            "No online Tailscale devices are visible. Check that your servers are connected, then Refresh; or go Back to enter an address."
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
        put_text(
            buffer,
            inner.x,
            y,
            inner.width.saturating_sub(status_width + 1),
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
        hits.push((rect, Hit::TailscaleDevice(index)));
        y = y.saturating_add(rows + 1);
    }
    let refresh = Rect::new(inner.x, inner.bottom() - 2, inner.width.min(24), 1);
    put_text(
        buffer,
        refresh.x,
        refresh.y,
        refresh.width,
        if browser.loading {
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
    (search_width > 0).then_some(crate::protocol::CursorState {
        x: inner.x + 8 + cursor_col.min(search_width.saturating_sub(1)),
        y: inner.y + 2,
        visible: true,
        shape: 6,
    })
}
