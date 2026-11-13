use super::*;

fn footer_rows(area: Rect) -> u16 {
    if area.width < 3 || area.height < 4 {
        0
    } else if area.height < 5 {
        1
    } else if area.height < 12 {
        2
    } else if area.width >= 21 && area.height >= 24 {
        7
    } else {
        3
    }
}
pub(super) fn cards_visible(area: Rect) -> bool {
    footer_rows(area) == 7
}
pub(super) fn content_area(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(footer_rows(area)),
    )
}
fn version_text(width: u16) -> &'static str {
    if crate::build_info::DISPLAY_VERSION.len() <= usize::from(width) {
        crate::build_info::DISPLAY_VERSION
    } else if crate::build_info::COMPACT_VERSION.len() <= usize::from(width) {
        crate::build_info::COMPACT_VERSION
    } else {
        "v"
    }
}

pub(super) fn paint(
    buffer: &mut Buffer,
    area: Rect,
    view: &crate::client::sync_status::View,
    palette: &Palette,
    hits: &mut ShellHitMap,
) {
    hits.sync_status = Rect::default();
    if content_area(area) == area {
        return;
    }
    let version = Rect::new(area.x, area.bottom() - 1, area.width.saturating_sub(1), 1);
    render::put_text(
        buffer,
        version.x,
        version.y,
        version.width,
        version_text(version.width),
        Style::default().fg(palette.overlay0).bg(palette.sidebar_bg),
    );
    if footer_rows(area) < 2 {
        return;
    }
    let row = Rect::new(
        area.x,
        area.bottom() - footer_rows(area).min(3),
        area.width.saturating_sub(1),
        1,
    );
    use crate::client::sync_status::Level;
    let color = match view.level {
        Level::Good => palette.green,
        Level::Waiting => palette.yellow,
        Level::Warning => palette.red,
        Level::Off => palette.overlay0,
    };
    buffer.set_style(row, Style::default().bg(palette.sidebar_bg).fg(color));
    let text = if row.width < 12 {
        &view.compact
    } else {
        &view.badge
    };
    render::put_text(
        buffer,
        row.x,
        row.y,
        row.width,
        text,
        Style::default().bg(palette.sidebar_bg).fg(color),
    );
    hits.sync_status = row;
    if footer_rows(area) >= 3 {
        let updates = Rect::new(row.x, row.y + 1, row.width, 1);
        let color = match view.update_level {
            Level::Good => palette.green,
            Level::Warning => palette.red,
            _ => palette.yellow,
        };
        render::put_text(
            buffer,
            updates.x,
            updates.y,
            updates.width,
            if updates.width < 12 {
                "↑"
            } else {
                &view.updates
            },
            Style::default().fg(color).bg(palette.sidebar_bg),
        );
        hits.sync_status.height = 2;
    }
    if cards_visible(area) {
        let start = area.bottom() - 7;
        for index in 0..2 {
            let y = start + index * 2;
            let manager = view.managers.get(usize::from(index));
            let color = match manager.map(|m| m.level) {
                Some(Level::Good) => palette.green,
                Some(Level::Warning) => palette.red,
                _ => palette.yellow,
            };
            let name = manager
                .map(|m| m.name.as_str())
                .unwrap_or("Manager · waiting…");
            let full = manager
                .map(|m| m.status.as_str())
                .unwrap_or("? Version unknown · auto ?");
            let compact = manager
                .map(|m| m.compact.as_str())
                .unwrap_or("unknown auto ?");
            let seen = manager.map(|m| m.last_seen.as_str()).unwrap_or("?");
            let age_width = seen.len().min(usize::from(row.width)) as u16;
            let status_width = row.width.saturating_sub(age_width + 1);
            let inline = compact.len() <= usize::from(status_width);
            let name_width = if inline { row.width } else { status_width };
            render::put_text(
                buffer,
                row.x,
                y,
                name_width,
                name,
                Style::default().fg(palette.text).bg(palette.sidebar_bg),
            );
            let status_width = if inline { status_width } else { row.width };
            let status = if full.len() <= usize::from(status_width) {
                full
            } else {
                compact
            };
            render::put_text(
                buffer,
                row.x,
                y + 1,
                status_width,
                status,
                Style::default().fg(color).bg(palette.sidebar_bg),
            );
            render::put_text(
                buffer,
                row.x + row.width - age_width,
                if inline { y + 1 } else { y },
                age_width,
                seen,
                Style::default()
                    .fg(if manager.is_some_and(|m| m.level == Level::Good) {
                        palette.overlay0
                    } else {
                        color
                    })
                    .bg(palette.sidebar_bg),
            );
        }
        hits.sync_status.y = start;
        hits.sync_status.height = 6;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserved_row_stays_outside_content_for_expanded_and_collapsed_sidebars() {
        for width in [3, 5, 25] {
            let area = Rect::new(2, 3, width, 20);
            let content = content_area(area);
            assert_eq!(content.bottom(), area.bottom() - 3);
            assert_eq!(content.width, area.width);
        }
        for area in [
            Rect::new(0, 0, 0, 20),
            Rect::new(0, 0, 2, 20),
            Rect::new(0, 0, 25, 3),
        ] {
            assert_eq!(content_area(area), area);
        }
    }
    #[test]
    fn version_is_on_last_row_and_sync_hit_is_above_it() {
        for width in [3, 4, 26] {
            let area = Rect::new(0, 0, width, 12);
            let mut buffer = Buffer::empty(area);
            let mut hits = ShellHitMap::default();
            paint(
                &mut buffer,
                area,
                &Default::default(),
                &Palette::catppuccin(),
                &mut hits,
            );
            assert_eq!(hits.sync_status.y, area.bottom() - 3);
            assert_eq!(hits.sync_status.height, 2);
            let text = version_text(width - 1);
            let drawn: String = (0..width - 1)
                .map(|x| buffer[(x, area.bottom() - 1)].symbol())
                .collect();
            assert!(drawn.starts_with(text));
            assert!(content_area(area).bottom() <= hits.sync_status.y);
        }
    }
    #[test]
    fn short_sidebars_keep_agent_space_and_hide_only_the_update_summary() {
        let area = Rect::new(0, 0, 26, 9);
        let mut buffer = Buffer::empty(area);
        let mut hits = ShellHitMap::default();
        paint(
            &mut buffer,
            area,
            &Default::default(),
            &Palette::catppuccin(),
            &mut hits,
        );
        assert_eq!(content_area(area).height, 7);
        assert_eq!(hits.sync_status.height, 1);
        assert_eq!(hits.sync_status.bottom(), area.bottom() - 1);
    }
    #[test]
    fn both_managers_are_visible_above_summary_without_covering_version_or_content() {
        let area = Rect::new(0, 0, 26, 30);
        let mut buffer = Buffer::empty(area);
        let mut hits = ShellHitMap::default();
        let view = crate::client::sync_status::View {
            managers: ["David’s Mac Studio", "David’s MacBook Pro"]
                .into_iter()
                .map(|name| crate::client::sync_status::ManagerRow {
                    name: name.into(),
                    status: "✓ v0.9.5 b3 · auto on".into(),
                    compact: "b3 auto on".into(),
                    last_seen: "35s".into(),
                    level: crate::client::sync_status::Level::Good,
                })
                .collect(),
            ..Default::default()
        };
        paint(&mut buffer, area, &view, &Palette::catppuccin(), &mut hits);
        let line = |y| {
            (0..area.width - 1)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        };
        assert!(line(23).starts_with("David’s Mac Studio"));
        assert!(line(24).starts_with("b3 auto on"));
        assert!(line(24).trim_end().ends_with("35s"));
        assert!(line(25).starts_with("David’s MacBook Pro"));
        assert!(line(26).starts_with("b3 auto on"));
        assert!(line(26).trim_end().ends_with("35s"));
        assert_eq!(content_area(area).bottom(), 23);
        assert_eq!(hits.sync_status.y, 23);
        assert_eq!(hits.sync_status.bottom(), 29);
        assert!(line(29).starts_with(version_text(25)));
    }
    #[test]
    fn narrow_cards_preserve_complete_build_number_and_auto_setting() {
        let area = Rect::new(0, 0, 21, 30);
        for auto in ["on", "off", "?"] {
            let mut buffer = Buffer::empty(area);
            let mut hits = ShellHitMap::default();
            let mut view = crate::client::sync_status::View::default();
            let compact = format!("b4294967295 auto {auto}");
            view.managers.push(crate::client::sync_status::ManagerRow {
                name: "Laptop".into(),
                status: format!("? v0.9.5 b4294967295 · auto {auto}"),
                compact: compact.clone(),
                last_seen: "3m stale".into(),
                level: crate::client::sync_status::Level::Waiting,
            });
            paint(&mut buffer, area, &view, &Palette::catppuccin(), &mut hits);
            let drawn: String = (0..area.width - 1)
                .map(|x| buffer[(x, 24)].symbol())
                .collect();
            assert!(drawn.starts_with(&compact), "{drawn}");
            let name: String = (0..area.width - 1)
                .map(|x| buffer[(x, 23)].symbol())
                .collect();
            assert!(name.trim_end().ends_with("3m stale"));
        }
        assert_eq!(footer_rows(Rect::new(0, 0, 20, 30)), 3);
    }
    #[test]
    fn tiny_height_preserves_version_without_a_stale_sync_hit() {
        let area = Rect::new(0, 0, 26, 4);
        let mut buffer = Buffer::empty(area);
        let mut hits = ShellHitMap {
            sync_status: Rect::new(0, 0, 20, 1),
            ..Default::default()
        };
        paint(
            &mut buffer,
            area,
            &Default::default(),
            &Palette::catppuccin(),
            &mut hits,
        );
        assert!(hits.sync_status.is_empty());
        assert_eq!(content_area(area).height, 3);
        assert_eq!(buffer[(0, 3)].symbol(), "v");
    }
}
