use super::*;

fn footer_rows(area: Rect) -> u16 {
    if area.width < 3 || area.height < 4 {
        0
    } else if area.height < 5 {
        1
    } else {
        2
    }
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
    let row = Rect::new(area.x, area.bottom() - 2, area.width.saturating_sub(1), 1);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserved_row_stays_outside_content_for_expanded_and_collapsed_sidebars() {
        for width in [3, 5, 25] {
            let area = Rect::new(2, 3, width, 20);
            let content = content_area(area);
            assert_eq!(content.bottom(), area.bottom() - 2);
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
            assert_eq!(hits.sync_status.y, area.bottom() - 2);
            assert_eq!(hits.sync_status.height, 1);
            let text = version_text(width - 1);
            let drawn: String = (0..width - 1)
                .map(|x| buffer[(x, area.bottom() - 1)].symbol())
                .collect();
            assert!(drawn.starts_with(text));
            assert!(content_area(area).bottom() <= hits.sync_status.y);
        }
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
