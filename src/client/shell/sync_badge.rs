use super::*;

pub(super) fn content_area(area: Rect) -> Rect {
    if area.width >= 3 && area.height >= 4 {
        Rect::new(area.x, area.y, area.width, area.height - 1)
    } else {
        area
    }
}

pub(super) fn paint(
    buffer: &mut Buffer,
    area: Rect,
    view: &crate::client::sync_status::View,
    palette: &Palette,
    hits: &mut ShellHitMap,
) {
    if content_area(area) == area {
        return;
    }
    let row = Rect::new(area.x, area.bottom() - 1, area.width.saturating_sub(1), 1);
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
            assert_eq!(content.bottom(), area.bottom() - 1);
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
}
