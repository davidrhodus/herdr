use crate::{
    screen::{Cell as GridCell, Row},
    *,
};
use std::{cell::Cell, sync::Arc};

pub struct RenderState {
    rows: Vec<Arc<Row>>,
    styles: Arc<Vec<CellStyle>>,
    dirty_rows: Vec<Cell<bool>>,
    dirty: Dirty,
    cols: u16,
    height: u16,
    cursor: RenderCursor,
    colors: RenderColors,
    screen: ActiveScreen,
    layout: u64,
}
impl RenderState {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            rows: Vec::new(),
            styles: Arc::new(vec![CellStyle::default()]),
            dirty_rows: Vec::new(),
            dirty: Dirty::Full,
            cols: 0,
            height: 0,
            cursor: RenderCursor {
                viewport: None,
                visible: true,
                blinking: true,
                visual_style: CursorVisualStyle::Block,
            },
            colors: RenderColors {
                background: RgbColor::default(),
                foreground: RgbColor {
                    r: 255,
                    g: 255,
                    b: 255,
                },
                palette: default_palette(),
            },
            screen: ActiveScreen::Primary,
            layout: 0,
        })
    }
    pub fn update(&mut self, terminal: &Terminal) -> Result<(), Error> {
        self.styles = Arc::clone(&terminal.styles);
        let s = terminal.screen();
        let top = s.viewport_top();
        let screen = terminal.active_screen()?;
        let mut colors = RenderColors {
            background: terminal.bg.unwrap_or_default(),
            foreground: terminal.fg.unwrap_or(RgbColor {
                r: 255,
                g: 255,
                b: 255,
            }),
            palette: terminal.palette,
        };
        if terminal.mode_get(5)? {
            std::mem::swap(&mut colors.background, &mut colors.foreground);
        }
        let full = self.cols as usize != s.cols
            || self.height as usize != s.rows
            || self.screen != screen
            || self.layout != s.layout_revision
            || self.colors != colors;
        self.cols = s.cols as u16;
        self.height = s.rows as u16;
        self.screen = screen;
        self.layout = s.layout_revision;
        self.colors = colors;
        let mut changed = false;
        self.dirty_rows.resize_with(s.rows, || Cell::new(false));
        for (y, row) in s.lines.iter().skip(top).take(s.rows).enumerate() {
            let different = self
                .rows
                .get(y)
                .is_none_or(|old| old.version != row.version || !Arc::ptr_eq(old, row));
            if full || different {
                self.dirty_rows[y].set(true);
                changed = true;
            }
            if let Some(old) = self.rows.get_mut(y) {
                if different {
                    *old = Arc::clone(row);
                }
            } else {
                self.rows.push(Arc::clone(row));
            }
        }
        self.rows.truncate(s.rows);
        let absolute = s.history() + s.y;
        let viewport = (absolute >= top && absolute < top + s.rows).then_some(CursorViewport {
            x: s.x as u16,
            y: absolute.saturating_sub(top) as u16,
            wide_tail: s
                .lines
                .get(absolute)
                .and_then(|r| r.cells.get(s.x))
                .is_some_and(|c| c.wide == CellWide::SpacerTail),
        });
        let cursor = RenderCursor {
            viewport,
            visible: terminal.mode_get(25)?,
            blinking: terminal.blinking,
            visual_style: terminal.cursor_style,
        };
        let cursor_changed = self.cursor != cursor;
        self.cursor = cursor;
        if full {
            self.dirty = Dirty::Full;
        } else if (changed || cursor_changed) && self.dirty == Dirty::Clean {
            self.dirty = Dirty::Partial;
        }
        Ok(())
    }
    pub fn cols(&self) -> Result<u16, Error> {
        Ok(self.cols)
    }
    pub fn rows(&self) -> Result<u16, Error> {
        Ok(self.height)
    }
    pub fn dirty(&self) -> Result<Dirty, Error> {
        Ok(self.dirty)
    }
    pub fn cursor(&self) -> Result<RenderCursor, Error> {
        Ok(self.cursor)
    }
    pub fn colors(&self) -> Result<RenderColors, Error> {
        Ok(self.colors)
    }
    pub fn clean(&mut self) -> Result<(), Error> {
        for row in &self.dirty_rows {
            row.set(false);
        }
        self.set_dirty(Dirty::Clean)
    }
    pub fn set_dirty(&mut self, dirty: Dirty) -> Result<(), Error> {
        self.dirty = dirty;
        Ok(())
    }
    pub fn populate_row_iterator<'a>(
        &'a self,
        iterator: &'a mut RowIterator,
    ) -> Result<RowIter<'a>, Error> {
        iterator.next = 0;
        Ok(RowIter {
            state: self,
            iterator,
            current: None,
        })
    }
}
pub struct RowIterator {
    next: usize,
}
impl RowIterator {
    pub fn new() -> Result<Self, Error> {
        Ok(Self { next: 0 })
    }
}
pub struct RowIter<'a> {
    state: &'a RenderState,
    iterator: &'a mut RowIterator,
    current: Option<usize>,
}
impl RowIter<'_> {
    // This cursor exposes additional row operations between advances.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> bool {
        if self.iterator.next >= self.state.rows.len() {
            return false;
        }
        self.current = Some(self.iterator.next);
        self.iterator.next += 1;
        true
    }
    pub fn next_dirty(&mut self) -> Option<u16> {
        while self.next() {
            let y = self.current?;
            if self.state.dirty == Dirty::Full || self.state.dirty_rows[y].get() {
                return Some(y as u16);
            }
        }
        None
    }
    pub fn dirty(&self) -> Result<bool, Error> {
        Ok(self.current.is_some_and(|y| self.state.dirty_rows[y].get()))
    }
    pub fn wrap_state(&self) -> Result<(bool, bool), Error> {
        let row = &self.state.rows[self.current.ok_or(Error(-2))?];
        Ok((row.wrapped, row.continuation))
    }
    pub fn clear_dirty(&mut self) -> Result<(), Error> {
        self.set_dirty(false)
    }
    pub fn set_dirty(&mut self, dirty: bool) -> Result<(), Error> {
        self.state.dirty_rows[self.current.ok_or(Error(-2))?].set(dirty);
        Ok(())
    }
    pub fn selection(&self) -> Result<Option<RowSelection>, Error> {
        Ok(None)
    }
    pub fn populate_cells<'b>(
        &'b mut self,
        _cells: &'b mut RowCells,
    ) -> Result<RowCellIter<'b>, Error> {
        Ok(RowCellIter {
            row: &self.state.rows[self.current.ok_or(Error(-2))?],
            palette: &self.state.colors.palette,
            styles: &self.state.styles,
            cols: self.state.cols as usize,
            next: 0,
            current: None,
        })
    }
}
pub struct RowCells;
impl RowCells {
    pub fn new() -> Result<Self, Error> {
        Ok(Self)
    }
}
pub struct RowCellIter<'a> {
    row: &'a Row,
    cols: usize,
    palette: &'a [RgbColor; 256],
    styles: &'a [CellStyle],
    next: usize,
    current: Option<usize>,
}
impl RowCellIter<'_> {
    // This cursor supports selection and batched cell reads between advances.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> bool {
        if self.next >= self.cols {
            return false;
        }
        self.current = Some(self.next);
        self.next += 1;
        true
    }
    pub fn select(&mut self, x: u16) -> Result<(), Error> {
        if x as usize >= self.cols {
            return Err(Error(-2));
        }
        self.current = Some(x as usize);
        self.next = x as usize + 1;
        Ok(())
    }
    fn cell(&self) -> Result<&GridCell, Error> {
        Ok(self.row.cell(self.current.ok_or(Error(-2))?))
    }
    pub fn is_default_blank(&self) -> Result<bool, Error> {
        let c = self.cell()?;
        Ok(!c.meaningful() && c.wide == CellWide::Narrow)
    }
    pub fn basic_data(&self) -> Result<CellBasicData, Error> {
        let c = self.cell()?;
        Ok(CellBasicData {
            wide: c.wide,
            has_hyperlink: c.link != 0,
            has_styling: c.style != 0,
            style: self.styles[c.style as usize],
        })
    }
    pub fn wide(&self) -> Result<CellWide, Error> {
        Ok(self.cell()?.wide)
    }
    pub fn has_hyperlink(&self) -> Result<bool, Error> {
        Ok(self.cell()?.link != 0)
    }
    pub fn style(&self) -> Result<CellStyle, Error> {
        Ok(self.styles[self.cell()?.style as usize])
    }
    pub fn content_bg_color(&self) -> Result<Option<CellColor>, Error> {
        let c = self.cell()?;
        Ok(if !c.has_text() {
            self.styles[c.style as usize].bg_color
        } else {
            None
        })
    }
    fn color(&self, color: Option<CellColor>) -> Option<RgbColor> {
        color.map(|c| match c {
            CellColor::Palette(i) => self.palette[i as usize],
            CellColor::Rgb(c) => c,
        })
    }
    pub fn fg_color(&self) -> Result<Option<RgbColor>, Error> {
        Ok(self.color(self.style()?.fg_color))
    }
    pub fn bg_color(&self) -> Result<Option<RgbColor>, Error> {
        Ok(self.color(self.style()?.bg_color))
    }
    pub fn grapheme_text(&self) -> Result<String, Error> {
        Ok(self.row.string(self.cell()?))
    }
    pub fn grapheme_text_into(&self, _bytes: &mut Vec<u8>, text: &mut String) -> Result<(), Error> {
        text.clear();
        let c = self.cell()?;
        self.row.push_text(c, text);
        Ok(())
    }
}
pub(crate) fn style_ansi(style: CellStyle) -> String {
    let mut parts = vec!["0".to_string()];
    for (set, value) in [
        (style.bold, 1),
        (style.faint, 2),
        (style.italic, 3),
        (style.blink, 5),
        (style.inverse, 7),
        (style.invisible, 8),
        (style.strikethrough, 9),
        (style.overline, 53),
    ] {
        if set {
            parts.push(value.to_string());
        }
    }
    if style.underline > 0 {
        parts.push(format!("4:{}", style.underline));
    }
    for (color, base) in [
        (style.fg_color, 38),
        (style.bg_color, 48),
        (style.underline_color, 58),
    ] {
        match color {
            Some(CellColor::Palette(i)) => parts.push(format!("{base};5;{i}")),
            Some(CellColor::Rgb(c)) => parts.push(format!("{base};2;{};{};{}", c.r, c.g, c.b)),
            None => {}
        }
    }
    format!("\x1b[{}m", parts.join(";"))
}
