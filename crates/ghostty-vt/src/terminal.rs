use crate::{
    screen::{Cell, SavedCursor, Screen},
    *,
};
use base64::Engine as _;
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};
use vte::Perform;

type TokenSelection = (String, usize, Vec<(usize, usize, usize)>);
type Writer = Box<dyn FnMut(&[u8]) + Send>;
pub struct Terminal {
    pub(crate) primary: Screen,
    pub(crate) alternate: Screen,
    pub(crate) alt: bool,
    parser: Option<Box<vte::Parser<262144>>>,
    pub(crate) mouse_modes: crate::mouse::Modes,
    modes: HashMap<u16, bool>,
    hot_modes: [bool; 4],
    saved_modes: HashMap<u16, bool>,
    pub(crate) style: CellStyle,
    style_id: u16,
    erase_style_id: u16,
    pub(crate) styles: Arc<Vec<CellStyle>>,
    style_cache: HashMap<CellStyle, u16>,
    style_free: Vec<u16>,
    recent_style: Option<(CellStyle, u16)>,
    link_id: u16,
    links: Vec<Arc<str>>,
    link_cache: HashMap<Arc<str>, u16>,
    link_free: Vec<u16>,
    link_bytes: usize,
    link: Option<Arc<str>>,
    pub(crate) palette: [RgbColor; 256],
    defaults: [RgbColor; 256],
    pub(crate) fg: Option<RgbColor>,
    pub(crate) bg: Option<RgbColor>,
    pub(crate) cursor_color: Option<RgbColor>,
    pub(crate) cursor_style: CursorVisualStyle,
    pub(crate) blinking: bool,
    color_scheme: Option<ColorScheme>,
    write_pty: Option<Writer>,
    bells: u16,
    pwd: Vec<Vec<u8>>,
    clipboard: Vec<Vec<u8>>,
    pub(crate) cell_width: u32,
    pub(crate) cell_height: u32,
    keyboard: [Vec<u8>; 2],
    other_keys: bool,
    tabs: BTreeSet<usize>,
    charset: u8,
    last: Option<(usize, usize, u32, u8)>,
    grapheme: crate::unicode::State,
    pending_osc: Option<Vec<Vec<u8>>>,
    dcs: Vec<u8>,
    dcs_kind: Option<char>,
    in_apc: bool,
    apc_escape: bool,
    apc_overflow: bool,
    apc: Vec<u8>,
    pub(crate) graphics: crate::graphics::Graphics,
    pub(crate) inactive_graphics: crate::graphics::Graphics,
    activity: u64,
    compression_cursor: usize,
    pub(crate) kitty_fingerprints: std::sync::Mutex<HashMap<u32, FingerprintEntry>>,
    pub(crate) kitty_empty_generation: std::cell::Cell<Option<u64>>,
}
impl Terminal {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize) -> Result<Self, Error> {
        if cols == 0 || rows == 0 || u32::from(cols) * u32::from(rows) > 1_048_576 {
            return Err(Error(-2));
        }
        let palette = default_palette();
        let mut result = Self {
            primary: Screen::new(cols as usize, rows as usize, max_scrollback),
            alternate: Screen::new(cols as usize, rows as usize, 0),
            alt: false,
            parser: Some(Box::new(vte::Parser::new_with_size())),
            mouse_modes: crate::mouse::Modes::default(),
            modes: HashMap::new(),
            hot_modes: [true, false, true, false],
            saved_modes: HashMap::new(),
            style: CellStyle::default(),
            style_id: 0,
            erase_style_id: 0,
            styles: Arc::new(vec![CellStyle::default()]),
            style_cache: HashMap::from([(CellStyle::default(), 0)]),
            style_free: Vec::new(),
            recent_style: None,
            link_id: 0,
            links: vec![Arc::from("")],
            link_cache: HashMap::new(),
            link_free: Vec::new(),
            link_bytes: 0,
            link: None,
            palette,
            defaults: palette,
            fg: None,
            bg: None,
            cursor_color: None,
            cursor_style: CursorVisualStyle::Block,
            blinking: true,
            color_scheme: None,
            write_pty: None,
            bells: 0,
            pwd: Vec::new(),
            clipboard: Vec::new(),
            cell_width: 0,
            cell_height: 0,
            keyboard: [vec![0], vec![0]],
            other_keys: false,
            tabs: (8..cols as usize).step_by(8).collect(),
            charset: 0,
            last: None,
            grapheme: crate::unicode::State::default(),
            pending_osc: None,
            dcs: Vec::new(),
            dcs_kind: None,
            in_apc: false,
            apc_escape: false,
            apc_overflow: false,
            apc: Vec::new(),
            graphics: crate::graphics::Graphics::default(),
            inactive_graphics: crate::graphics::Graphics::default(),
            activity: 0,
            compression_cursor: 0,
            kitty_fingerprints: std::sync::Mutex::new(HashMap::new()),
            kitty_empty_generation: std::cell::Cell::new(Some(0)),
        };
        result.reset_modes();
        Ok(result)
    }
    pub(crate) fn screen(&self) -> &Screen {
        if self.alt {
            &self.alternate
        } else {
            &self.primary
        }
    }
    pub(crate) fn screen_mut(&mut self) -> &mut Screen {
        if self.alt {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }
    fn reset_modes(&mut self) {
        self.hot_modes = [true, false, true, false];
        self.modes.clear();
        for mode in [7, 25, 1007, 1035, 1036, 2027] {
            self.modes.insert(mode, true);
        }
        self.mouse_modes = crate::mouse::Modes::default();
    }
    fn mode(&self, mode: u16) -> bool {
        match mode {
            7 => self.hot_modes[0],
            0x8004 => self.hot_modes[1],
            2027 => self.hot_modes[2],
            6 => self.hot_modes[3],
            _ => self.modes.get(&mode).copied().unwrap_or(false),
        }
    }
    pub(crate) fn reply(&mut self, bytes: &[u8]) {
        if let Some(write) = &mut self.write_pty {
            write(bytes);
        }
    }
    fn moved(&mut self) {
        self.last = None;
        self.grapheme = crate::unicode::State::default();
        self.screen_mut().wrap_pending = false;
    }
    fn flush_graphics_scrolls(&mut self) {
        let (primary, alternate) = if self.alt {
            (&mut self.inactive_graphics, &mut self.graphics)
        } else {
            (&mut self.graphics, &mut self.inactive_graphics)
        };
        primary.scroll(&self.primary.scrolls);
        alternate.scroll(&self.alternate.scrolls);
        self.primary.scrolls.clear();
        self.alternate.scrolls.clear();
    }
    pub fn write(&mut self, mut bytes: &[u8]) {
        self.activity = self.activity.wrapping_add(1);
        let Some(mut parser) = self.parser.take() else {
            return;
        };
        while !bytes.is_empty() {
            if self.in_apc {
                if self.apc_escape {
                    self.apc_escape = false;
                    let byte = bytes[0];
                    bytes = &bytes[1..];
                    if byte == b'\\' {
                        parser.advance(self, b"\x1b\\");
                        self.in_apc = false;
                        self.flush_graphics_scrolls();
                        let payload = std::mem::take(&mut self.apc);
                        if !self.apc_overflow {
                            let mut graphics = std::mem::take(&mut self.graphics);
                            graphics.command(self, &payload);
                            self.graphics = graphics;
                        }
                        self.apc_overflow = false;
                    } else {
                        self.in_apc = false;
                        self.apc.clear();
                        parser.advance(self, &[0x1b, byte]);
                    }
                } else {
                    let end = memchr::memchr3(0x1b, 0x18, 0x1a, bytes).unwrap_or(bytes.len());
                    if self.apc.len().saturating_add(end) > 16 * 1024 * 1024 {
                        self.apc_overflow = true;
                    }
                    if !self.apc_overflow {
                        self.apc.extend_from_slice(&bytes[..end]);
                    }
                    bytes = &bytes[end..];
                    if !bytes.is_empty() {
                        if matches!(bytes[0], 0x18 | 0x1a) {
                            let byte = bytes[0];
                            self.in_apc = false;
                            self.apc.clear();
                            parser.advance(self, &[byte]);
                        } else {
                            self.apc_escape = true;
                        }
                        bytes = &bytes[1..];
                    }
                }
            } else {
                let consumed = parser.advance_until_terminated(self, bytes);
                bytes = &bytes[consumed..];
            }
        }
        self.flush_graphics_scrolls();
        self.parser = Some(parser);
    }
    pub fn compression_activity(&self) -> Result<u64, Error> {
        Ok(self.activity)
    }
    pub fn compress_incremental(&mut self) -> Result<TerminalCompressionResult, Error> {
        let end = (self.compression_cursor + 128).min(self.primary.history());
        for i in self.compression_cursor..end {
            let row = Arc::make_mut(&mut self.primary.lines[i]);
            let before = row.bytes();
            row.cells.compress();
            self.primary.history_bytes = self
                .primary
                .history_bytes
                .saturating_sub(before)
                .saturating_add(row.bytes());
        }
        self.compression_cursor = end;
        if end < self.primary.history() {
            return Ok(TerminalCompressionResult::Pending);
        }
        self.compression_cursor = 0;
        self.primary.trim_history();
        Ok(TerminalCompressionResult::Complete)
    }
    pub fn set_default_palette(&mut self, palette: &[RgbColor; 256]) -> Result<(), Error> {
        for (i, color) in palette.iter().enumerate() {
            if self.palette[i] == self.defaults[i] {
                self.palette[i] = *color;
            }
        }
        self.defaults = *palette;
        Ok(())
    }
    pub fn default_palette(&self) -> Result<[RgbColor; 256], Error> {
        Ok(self.palette)
    }
    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width: u32,
        cell_height: u32,
    ) -> Result<(), Error> {
        if cols == 0 || rows == 0 || u32::from(cols) * u32::from(rows) > 1_048_576 {
            return Err(Error(-2));
        }
        self.moved();
        let old_cols = self.primary.cols;
        let primary_anchors = if self.alt {
            &self.inactive_graphics
        } else {
            &self.graphics
        }
        .anchors(&self.primary);
        let alternate_anchors = if self.alt {
            &self.graphics
        } else {
            &self.inactive_graphics
        }
        .anchors(&self.alternate);
        let mut primary_points: Vec<_> = primary_anchors.iter().map(|p| p.1).collect();
        let mut alternate_points: Vec<_> = alternate_anchors.iter().map(|p| p.1).collect();
        self.primary
            .resize(cols as usize, rows as usize, true, &mut primary_points);
        self.alternate
            .resize(cols as usize, rows as usize, false, &mut alternate_points);
        if self.alt {
            self.inactive_graphics
                .remap(&primary_anchors, &primary_points, &self.primary);
            self.graphics
                .remap(&alternate_anchors, &alternate_points, &self.alternate);
        } else {
            self.graphics
                .remap(&primary_anchors, &primary_points, &self.primary);
            self.inactive_graphics
                .remap(&alternate_anchors, &alternate_points, &self.alternate);
        }
        self.cell_width = cell_width;
        self.cell_height = cell_height;
        self.tabs.retain(|x| *x < cols as usize);
        if cols as usize > old_cols {
            self.tabs
                .extend(((old_cols.div_ceil(8) * 8).max(8)..cols as usize).step_by(8));
        }
        if self.mode(2048) {
            self.report_size();
        }
        Ok(())
    }
    pub fn set_write_pty_callback<F: FnMut(&[u8]) + Send + 'static>(
        &mut self,
        callback: F,
    ) -> Result<(), Error> {
        self.write_pty = Some(Box::new(callback));
        Ok(())
    }
    pub fn set_color_scheme(&mut self, scheme: Option<ColorScheme>) -> Option<ColorScheme> {
        std::mem::replace(&mut self.color_scheme, scheme)
    }
    pub fn take_bell_count(&mut self) -> u16 {
        std::mem::take(&mut self.bells)
    }
    pub fn take_pwd_changes(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pwd)
    }
    pub fn take_clipboard_writes(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.clipboard)
    }
    pub fn mode_get(&self, mode: u16) -> Result<bool, Error> {
        Ok(self.mode(mode))
    }
    pub fn mode_set(&mut self, mode: u16, value: bool) -> Result<(), Error> {
        self.modes.insert(mode, value);
        match mode {
            7 => self.hot_modes[0] = value,
            0x8004 => self.hot_modes[1] = value,
            2027 => self.hot_modes[2] = value,
            6 => self.hot_modes[3] = value,
            _ => {}
        }
        self.mouse_modes.set_bit(mode, value);
        Ok(())
    }
    pub fn kitty_keyboard_flags(&self) -> Result<u8, Error> {
        Ok(*self.keyboard[self.alt as usize].last().unwrap_or(&0))
    }
    pub fn mouse_tracking_enabled(&self) -> Result<bool, Error> {
        Ok(self.mouse_modes.active())
    }
    pub fn modify_other_keys_enabled(&self) -> Result<bool, Error> {
        Ok(self.other_keys)
    }
    pub fn active_screen(&self) -> Result<ActiveScreen, Error> {
        Ok(if self.alt {
            ActiveScreen::Alternate
        } else {
            ActiveScreen::Primary
        })
    }
    pub fn total_rows(&self) -> Result<usize, Error> {
        Ok(self.screen().lines.len())
    }
    pub fn scrollback_rows(&self) -> Result<usize, Error> {
        Ok(self.screen().history())
    }
    pub fn max_scrollback(&self) -> usize {
        self.primary.budget
    }
    pub fn scrollbar(&self) -> Result<TerminalScrollbar, Error> {
        let s = self.screen();
        Ok(TerminalScrollbar {
            total: s.lines.len(),
            offset: s.viewport_top(),
            len: s.rows,
        })
    }
    pub fn track_row(&mut self, y: u32) -> Option<usize> {
        ((y as usize) < self.screen().lines.len()).then_some(y as usize)
    }
    pub fn cols(&self) -> Result<u16, Error> {
        Ok(self.screen().cols as u16)
    }
    pub fn rows(&self) -> Result<u16, Error> {
        Ok(self.screen().rows as u16)
    }
    pub fn cursor_y(&self) -> Result<u16, Error> {
        Ok(self.screen().y as u16)
    }
    pub fn effective_foreground_color(&self) -> Result<Option<RgbColor>, Error> {
        Ok(self.fg)
    }
    pub fn effective_cursor_color(&self) -> Result<Option<RgbColor>, Error> {
        Ok(self.cursor_color)
    }
    pub fn width_px(&self) -> Result<u32, Error> {
        Ok(self.screen().cols as u32 * self.cell_width)
    }
    pub fn height_px(&self) -> Result<u32, Error> {
        Ok(self.screen().rows as u32 * self.cell_height)
    }
    pub fn scroll_viewport_bottom(&mut self) {
        let s = self.screen_mut();
        if s.offset != 0 {
            s.offset = 0;
            s.layout_revision = s.layout_revision.wrapping_add(1);
        }
    }
    pub fn scroll_viewport_delta(&mut self, delta: isize) {
        let s = self.screen_mut();
        s.offset = s.offset.saturating_add_signed(-delta).min(s.history());
        s.layout_revision = s.layout_revision.wrapping_add(1);
    }
    pub fn scroll_viewport_row(&mut self, row: usize) {
        let s = self.screen_mut();
        s.offset = s.history().saturating_sub(row.min(s.history()));
        s.layout_revision = s.layout_revision.wrapping_add(1);
    }
    pub fn clear_screen(&mut self) -> bool {
        if self.alt {
            return false;
        }
        let s = &mut self.primary;
        let cursor = s.history() + s.y;
        let mut start = cursor;
        let mut end = cursor;
        while start > 0 && s.lines[start].continuation {
            start -= 1;
        }
        while end + 1 < s.lines.len() && s.lines[end].wrapped {
            end += 1;
        }
        let kept = s
            .lines
            .iter()
            .skip(start)
            .take(end - start + 1)
            .cloned()
            .collect();
        s.lines = kept;
        s.dropped += start as u64;
        // A wrapped input line can extend into history. Cursor coordinates
        // remain relative to the live viewport after retaining that line.
        s.y = (cursor - start).saturating_sub(s.history()).min(s.rows - 1);
        s.offset = 0;
        s.history_bytes = 0;
        for row in &mut s.lines {
            Arc::make_mut(row).cells.resize(s.cols, Cell::default());
        }
        while s.lines.len() < s.rows {
            let row = s.blank(0);
            s.lines.push_back(row);
        }
        s.history_bytes = s
            .lines
            .iter()
            .take(s.history())
            .map(|row| row.bytes())
            .sum();
        s.trim_history();
        s.layout_revision = s.layout_revision.wrapping_add(1);
        self.last = None;
        true
    }
    pub(crate) fn cell(&self, x: usize, y: usize) -> Option<&Cell> {
        if x >= self.screen().cols {
            return None;
        }
        Some(self.screen().lines.get(y)?.cell(x))
    }
    pub fn screen_cell(&self, x: u16, y: u32) -> Result<(CellWide, Vec<u32>), Error> {
        let c = self.cell(x as usize, y as usize).ok_or(Error(-2))?;
        Ok((c.wide, self.screen().lines[y as usize].text(c).to_vec()))
    }
    pub fn screen_text_rows(&self) -> Result<Vec<ScreenTextRow>, Error> {
        self.screen_text_rows_range(0, usize::MAX)
    }
    pub fn screen_text_rows_range(
        &self,
        start: usize,
        end: usize,
    ) -> Result<Vec<ScreenTextRow>, Error> {
        Ok(self
            .screen()
            .lines
            .iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(|row| ScreenTextRow {
                cells: (0..self.screen().cols)
                    .map(|x| row.cell(x))
                    .map(|cell| ScreenTextCell {
                        wide: cell.wide,
                        graphemes: row.text(cell).to_vec(),
                    })
                    .collect(),
                soft_wrapped: row.wrapped,
                wrap_continuation: row.continuation,
            })
            .collect())
    }
    pub fn for_each_screen_row_cell(
        &self,
        row: u32,
        mut visit: impl FnMut(CellWide, &[u32]),
    ) -> Result<(), Error> {
        if let Some(row) = self.screen().lines.get(row as usize) {
            for x in 0..self.screen().cols {
                let cell = row.cell(x);
                visit(cell.wide, row.text(cell));
            }
        }
        Ok(())
    }
    pub fn viewport_hyperlink_uri(&self, x: u16, y: u32) -> Result<Option<String>, Error> {
        let row = self.screen().viewport_top() + y as usize;
        let mut col = x as usize;
        if self
            .cell(col, row)
            .is_some_and(|c| c.wide == CellWide::SpacerTail)
        {
            col = col.saturating_sub(1);
        }
        Ok(self
            .cell(col, row)
            .and_then(|cell| self.cell_link(cell))
            .map(|link| link.to_string()))
    }
    pub(crate) fn token(&self, x: u16, y: u32, budget: usize) -> Option<TokenSelection> {
        let s = self.screen();
        let mut col = x as usize;
        let row = s.viewport_top() + y as usize;
        let cell = self.cell(col, row)?;
        if cell.wide == CellWide::SpacerTail {
            col = col.saturating_sub(1);
        }
        let stop =
            |c: &Cell| !c.has_text() || char::from_u32(c.scalar[0]).is_none_or(char::is_whitespace);
        if stop(self.cell(col, row)?) {
            return None;
        }
        let mut start = (row, col);
        let mut remaining = budget;
        while remaining > 0 {
            remaining -= 1;
            let previous = if start.1 > 0 {
                Some((start.0, start.1 - 1))
            } else if start.0 > 0 && s.lines[start.0 - 1].wrapped {
                Some((start.0 - 1, s.cols - 1))
            } else {
                None
            };
            let Some(p) = previous else {
                break;
            };
            let c = self.cell(p.1, p.0)?;
            if !matches!(c.wide, CellWide::SpacerTail | CellWide::SpacerHead) && stop(c) {
                break;
            }
            start = p;
        }
        if remaining == 0 {
            return None;
        }
        let mut text = String::new();
        let mut clicked = 0;
        let mut cells = Vec::new();
        let mut p = start;
        loop {
            let c = self.cell(p.1, p.0)?;
            if c.wide != CellWide::SpacerTail && c.wide != CellWide::SpacerHead && stop(c) {
                break;
            }
            if remaining == 0 {
                return None;
            }
            remaining -= 1;
            if c.wide != CellWide::SpacerTail && c.wide != CellWide::SpacerHead {
                if stop(c) {
                    break;
                }
                if p == (row, col) {
                    clicked = text.len();
                }
                let begin = text.len();
                s.lines[p.0].push_text(c, &mut text);
                cells.push((p.0, p.1, begin));
            }
            if p.1 + 1 < s.cols {
                p.1 += 1;
            } else if s.lines[p.0].wrapped && p.0 + 1 < s.lines.len() {
                p = (p.0 + 1, 0);
            } else {
                break;
            }
        }
        Some((text, clicked, cells))
    }
    pub fn viewport_link_target(&self, x: u16, y: u32) -> Result<Option<LinkTarget>, Error> {
        if let Some(uri) = self.viewport_hyperlink_uri(x, y)? {
            return Ok(Some(LinkTarget::Uri(uri)));
        }
        Ok(self
            .token(x, y, 8192)
            .map(|(text, clicked_byte, _)| LinkTarget::Text { text, clicked_byte }))
    }
    pub fn viewport_link_regions(
        &self,
        x: u16,
        y: u32,
        resolve: fn(&str, usize) -> Option<std::ops::Range<usize>>,
    ) -> Result<Vec<LinkRegion>, Error> {
        if x as usize >= self.screen().cols
            || y as usize >= self.screen().rows
            || self.viewport_hyperlink_uri(x, y)?.is_some()
        {
            return Ok(Vec::new());
        }
        let Some((text, clicked, cells)) = self.token(x, y, 8192) else {
            return Ok(Vec::new());
        };
        let Some(range) = resolve(&text, clicked) else {
            return Ok(Vec::new());
        };
        let s = self.screen();
        let top = s.viewport_top();
        let mut out: Vec<LinkRegion> = Vec::new();
        for (index, &(row, col, start)) in cells.iter().enumerate() {
            let end = cells.get(index + 1).map_or(text.len(), |c| c.2);
            if end <= range.start || start >= range.end || row < top || row >= top + s.rows {
                continue;
            }
            let endcol = (col
                + usize::from(
                    self.cell(col, row)
                        .is_some_and(|c| c.wide == CellWide::Wide),
                ))
            .min(s.cols - 1) as u16;
            let y = (row - top) as u16;
            if let Some(last) = out
                .last_mut()
                .filter(|r| r.row == y && r.end_col + 1 == col as u16)
            {
                last.end_col = endcol;
            } else {
                out.push(LinkRegion {
                    row: y,
                    start_col: col as u16,
                    end_col: endcol,
                });
            }
        }
        Ok(out)
    }
    fn formatted(
        &self,
        mut start: (u16, u32),
        mut end: (u16, u32),
        rectangle: bool,
        ansi: bool,
        unwrap: bool,
    ) -> Result<String, Error> {
        let s = self.screen();
        if (start.1, start.0) > (end.1, end.0) {
            std::mem::swap(&mut start, &mut end);
        }
        let mut out = String::new();
        let mut last_style = CellStyle::default();
        let mut last_link: Option<Arc<str>> = None;
        let endrow = (end.1 as usize).min(s.lines.len().saturating_sub(1));
        for y in start.1 as usize..=endrow {
            let row = &s.lines[y];
            let left = if rectangle || y == start.1 as usize {
                start.0 as usize
            } else {
                0
            };
            let right = if rectangle || y == endrow {
                end.0 as usize
            } else {
                s.cols - 1
            };
            let mut line = String::new();
            let left = if left > 0
                && row
                    .cells
                    .get(left)
                    .is_some_and(|c| c.wide == CellWide::SpacerTail)
            {
                left - 1
            } else {
                left
            };
            let mut limit = right.min(s.cols - 1) + 1;
            if !row.wrapped || !unwrap {
                while limit > left
                    && !row.cell(limit - 1).has_text()
                    && self.cell_style(row.cell(limit - 1)).bg_color.is_none()
                {
                    limit -= 1;
                }
            }
            for x in left.min(s.cols)..limit.max(left).min(s.cols) {
                let c = row.cell(x);
                if matches!(c.wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                    continue;
                }
                let style = self.cell_style(c);
                let link = self.cell_link(c);
                if ansi {
                    if style != last_style {
                        line.push_str(&crate::render::style_ansi(style));
                        last_style = style;
                    }
                    if link != last_link.as_ref() {
                        line.push_str("\x1b]8;;");
                        if let Some(link) = link {
                            line.push_str(link);
                        }
                        line.push_str("\x1b\\");
                        last_link = link.cloned();
                    }
                }
                row.push_text(c, &mut line);
            }
            if !ansi && (!unwrap || !row.wrapped || rectangle) {
                line.truncate(line.trim_end_matches(' ').len());
            }
            out.push_str(&line);
            if y < endrow && (!row.wrapped || rectangle || !unwrap) {
                if ansi {
                    out.push_str("\r\n");
                } else {
                    out.push('\n');
                }
            }
        }
        if ansi {
            if last_link.is_some() {
                out.push_str("\x1b]8;;\x1b\\");
            }
            if last_style != CellStyle::default() {
                out.push_str("\x1b[0m");
            }
        }
        if out.trim().is_empty() {
            out.clear();
        }
        Ok(out)
    }
    pub fn read_text_viewport(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    ) -> Result<String, Error> {
        let base = self.screen().viewport_top() as u32;
        self.formatted(
            (start.0, start.1.saturating_add(base)),
            (end.0, end.1.saturating_add(base)),
            rectangle,
            false,
            true,
        )
    }
    pub fn read_ansi_viewport(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    ) -> Result<String, Error> {
        let base = self.screen().viewport_top() as u32;
        self.formatted(
            (start.0, start.1.saturating_add(base)),
            (end.0, end.1.saturating_add(base)),
            rectangle,
            true,
            false,
        )
    }
    pub fn read_text_screen(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
    ) -> Result<String, Error> {
        self.formatted(start, end, rectangle, false, true)
    }
    pub fn read_ansi_screen(
        &self,
        start: (u16, u32),
        end: (u16, u32),
        rectangle: bool,
        unwrap: bool,
    ) -> Result<String, Error> {
        self.formatted(start, end, rectangle, true, unwrap)
    }
    fn report_scheme(&mut self) {
        if let Some(s) = self.color_scheme {
            self.reply(
                format!("\x1b[?997;{}n", if s == ColorScheme::Dark { 1 } else { 2 }).as_bytes(),
            );
        }
    }
    fn report_size(&mut self) {
        self.reply(
            format!(
                "\x1b[48;{};{};{};{}t",
                self.screen().rows,
                self.screen().cols,
                self.height_px().unwrap_or(0),
                self.width_px().unwrap_or(0)
            )
            .as_bytes(),
        );
    }
    fn save_cursor(&mut self) {
        let saved = SavedCursor {
            x: self.screen().x,
            y: self.screen().y,
            style: self.style,
            origin: self.mode(6),
            wrap_pending: self.screen().wrap_pending,
        };
        self.screen_mut().saved = saved;
    }
    fn restore_cursor(&mut self) {
        let saved = self.screen().saved.clone();
        self.style = saved.style;
        self.sync_style();
        self.modes.insert(6, saved.origin);
        self.hot_modes[3] = saved.origin;
        let s = self.screen_mut();
        s.x = saved.x.min(s.cols - 1);
        s.y = saved.y.min(s.rows - 1);
        s.wrap_pending = saved.wrap_pending;
        self.last = None;
    }
    fn set_mode(&mut self, mode: u16, value: bool) {
        self.flush_graphics_scrolls();
        self.modes.insert(mode, value);
        match mode {
            7 => self.hot_modes[0] = value,
            0x8004 => self.hot_modes[1] = value,
            2027 => self.hot_modes[2] = value,
            6 => self.hot_modes[3] = value,
            _ => {}
        }
        self.mouse_modes.set(mode, value);
        match mode {
            6 => {
                let top = if value { self.screen().top } else { 0 };
                let s = self.screen_mut();
                s.x = 0;
                s.y = top;
                self.moved();
            }
            47 | 1047 | 1049 => {
                if value && !self.alt {
                    if mode == 1049 {
                        self.save_cursor();
                    }
                    let cols = self.primary.cols;
                    let rows = self.primary.rows;
                    if mode != 47 {
                        self.alternate = Screen::new(cols, rows, 0);
                    }
                    if mode != 47 {
                        self.inactive_graphics.reset();
                    }
                    std::mem::swap(&mut self.graphics, &mut self.inactive_graphics);
                    self.kitty_empty_generation.set(None);
                    self.alt = true;
                } else if !value && self.alt {
                    std::mem::swap(&mut self.graphics, &mut self.inactive_graphics);
                    self.kitty_empty_generation.set(None);
                    self.alt = false;
                    if mode == 1049 {
                        self.restore_cursor();
                    }
                }
                self.last = None;
                self.screen_mut().layout_revision = self.screen().layout_revision.wrapping_add(1);
            }
            1048 => {
                if value {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            2048 if value => {
                self.report_size();
            }
            _ => {}
        }
    }
    fn sgr(&mut self, params: &vte::Params) {
        if params.len() == 1 && params.as_single_params() == Some(&[0]) {
            self.style = CellStyle::default();
            self.style_id = 0;
            self.erase_style_id = 0;
            return;
        }
        if let Some([kind @ (38 | 48 | 58), 2, r, g, b]) = params.as_single_params() {
            let color = Some(CellColor::Rgb(RgbColor {
                r: (*r).min(255) as u8,
                g: (*g).min(255) as u8,
                b: (*b).min(255) as u8,
            }));
            match kind {
                38 => self.style.fg_color = color,
                48 => self.style.bg_color = color,
                _ => self.style.underline_color = color,
            }
            self.sync_style();
            return;
        }
        let mut parts = params.iter();
        while let Some(p) = parts.next() {
            let value = p[0];
            match value {
                0 => self.style = CellStyle::default(),
                1 => self.style.bold = true,
                2 => self.style.faint = true,
                3 => self.style.italic = true,
                4 => {
                    self.style.underline = p.get(1).copied().unwrap_or(1).min(5) as u8;
                    self.style.underlined = self.style.underline != 0;
                }
                5 | 6 => self.style.blink = true,
                7 => self.style.inverse = true,
                8 => self.style.invisible = true,
                9 => self.style.strikethrough = true,
                21 => {
                    self.style.underline = 2;
                    self.style.underlined = true;
                }
                22 => {
                    self.style.bold = false;
                    self.style.faint = false;
                }
                23 => self.style.italic = false,
                24 => {
                    self.style.underline = 0;
                    self.style.underlined = false;
                }
                25 => self.style.blink = false,
                27 => self.style.inverse = false,
                28 => self.style.invisible = false,
                29 => self.style.strikethrough = false,
                30..=37 => self.style.fg_color = Some(CellColor::Palette((value - 30) as u8)),
                40..=47 => self.style.bg_color = Some(CellColor::Palette((value - 40) as u8)),
                90..=97 => self.style.fg_color = Some(CellColor::Palette((value - 90 + 8) as u8)),
                100..=107 => {
                    self.style.bg_color = Some(CellColor::Palette((value - 100 + 8) as u8))
                }
                39 => self.style.fg_color = None,
                49 => self.style.bg_color = None,
                53 => self.style.overline = true,
                55 => self.style.overline = false,
                59 => self.style.underline_color = None,
                38 | 48 | 58 => {
                    let mut buffer = [0; 4];
                    let extra = if p.len() > 1 {
                        &p[1..]
                    } else {
                        let mut len = 0;
                        if let Some(mode) = parts.next() {
                            buffer[0] = mode[0];
                            len = 1;
                            let count = if buffer[0] == 2 { 4 } else { 2 };
                            for slot in &mut buffer[1..count] {
                                if let Some(p) = parts.next() {
                                    *slot = p[0];
                                    len += 1;
                                } else {
                                    break;
                                }
                            }
                        }
                        &buffer[..len]
                    };
                    let color = match extra.first() {
                        Some(5) => extra
                            .get(1)
                            .map(|v| CellColor::Palette((*v).min(255) as u8)),
                        Some(2) if extra.len() >= 4 => {
                            let n = extra.len();
                            Some(CellColor::Rgb(RgbColor {
                                r: extra[n - 3].min(255) as u8,
                                g: extra[n - 2].min(255) as u8,
                                b: extra[n - 1].min(255) as u8,
                            }))
                        }
                        _ => None,
                    };
                    match value {
                        38 => self.style.fg_color = color,
                        48 => self.style.bg_color = color,
                        _ => self.style.underline_color = color,
                    };
                }
                _ => {}
            }
        }
        self.sync_style();
    }
}
impl Perform for Terminal {
    fn apc_start(&mut self) {
        self.in_apc = true;
        self.apc.clear();
        self.apc_overflow = false;
    }
    fn terminated(&self) -> bool {
        self.in_apc
    }
    fn unhook_c1(&mut self) {
        // The application owns raw-C1 query replies; do not send a second one.
        self.dcs_kind = None;
        self.dcs.clear();
    }

    fn print_ascii(&mut self, mut bytes: &[u8]) {
        if self.charset != 0 || self.mode(0x8004) {
            for byte in bytes {
                self.print(char::from(*byte));
            }
            return;
        }
        if self.last.is_some_and(|(_, _, cp, _)| cp >= 128) {
            if let Some((&first, tail)) = bytes.split_first() {
                self.print(char::from(first));
                bytes = tail;
            }
        }
        let autowrap = self.mode(7);
        let style = self.style_id;
        let erase = self.erase_style_id;
        let link = self.link_id;
        while !bytes.is_empty() {
            let s = self.screen_mut();
            if s.wrap_pending {
                if autowrap {
                    s.live_row_mut(s.y).wrapped = true;
                    s.newline(erase);
                    s.x = 0;
                    s.live_row_mut(s.y).continuation = true;
                }
                s.wrap_pending = false;
            }
            if !autowrap && s.x == s.cols - 1 {
                bytes = &bytes[bytes.len() - 1..];
            }
            let (x, y, cols) = (s.x, s.y, s.cols);
            let n = bytes.len().min(cols - x);
            let row = s.live_row_mut(y);
            let cells = &mut row.cells;
            if x > 0 && cells[x].wide == CellWide::SpacerTail {
                cells[x - 1] = Cell::blank(erase);
            }
            if x + n < cols && cells[x + n - 1].wide == CellWide::Wide {
                cells[x + n] = Cell::blank(erase);
            }
            Cell::write_ascii(&mut cells[x..x + n], &bytes[..n], style, link);
            s.x = x + n;
            s.wrap_pending = s.x >= cols;
            if s.wrap_pending {
                s.x = cols - 1;
            }
            self.last = Some((x + n - 1, y, u32::from(bytes[n - 1]), 1));
            self.grapheme = crate::unicode::State::default();
            bytes = &bytes[n..];
        }
    }
    fn print(&mut self, mut character: char) {
        if self.charset == 1 {
            character = match character {
                'q' => '─',
                'x' => '│',
                'l' => '┌',
                'k' => '┐',
                'm' => '└',
                'j' => '┘',
                't' => '├',
                'u' => '┤',
                'v' => '┴',
                'w' => '┬',
                'n' => '┼',
                _ => character,
            };
        }
        let cp = character as u32;
        let mut width = unicode_codepoint_width(cp);
        if width as usize > self.screen().cols {
            self.last = None;
            return;
        }
        if let Some((x, y, previous, old_width)) = self.last {
            let joined = if self.mode(MODE_GRAPHEME_CLUSTER) {
                crate::unicode::append(previous, cp, &mut self.grapheme, old_width)
            } else if width == 0 {
                Some((old_width, true))
            } else {
                None
            };
            if let Some((new_width, store)) = joined {
                if !store {
                    return;
                }
                let erase = self.erase_style_id;
                let s = self.screen_mut();
                if y < s.rows && x < s.cols {
                    let cols = s.cols;
                    let row = s.live_row_mut(y);
                    if row.text(&row.cells[x]).len() >= 256 {
                        return;
                    }
                    row.append(x, cp);
                    if new_width == 2 && old_width == 1 && x + 1 == cols {
                        let cell = row.cells[x];
                        let cluster = row.cluster(&cell);
                        row.cells[x] = Cell {
                            wide: CellWide::SpacerHead,
                            ..Cell::default()
                        };
                        if cols > 1 {
                            row.wrapped = true;
                            s.newline(0);
                            s.x = 0;
                            s.wrap_pending = false;
                            let y = s.y;
                            let row = s.live_row_mut(y);
                            row.continuation = true;
                            if cols > 2 && row.cells[1].wide == CellWide::Wide {
                                row.cells[2] = Cell::blank(erase);
                            }
                            row.import(
                                0,
                                Cell {
                                    wide: CellWide::Wide,
                                    ..cell
                                },
                                cluster,
                            );
                            row.cells[1] = Cell {
                                wide: CellWide::SpacerTail,
                                ..Cell::default()
                            };
                            s.x = 2.min(cols - 1);
                            s.wrap_pending = cols == 2;
                            self.last = Some((0, y, cp, 2));
                        } else {
                            self.last = None;
                        }
                        return;
                    }
                    if new_width != old_width {
                        // A widened grapheme can cover the head of another
                        // wide glyph; retire its tail outside the new span.
                        if new_width == 2 && x + 2 < cols && row.cells[x + 1].wide == CellWide::Wide
                        {
                            row.cells[x + 2] = Cell::blank(erase);
                        }
                        row.cells[x].wide = if new_width == 2 {
                            CellWide::Wide
                        } else {
                            CellWide::Narrow
                        };
                        if x + 1 < cols {
                            row.cells[x + 1] = if new_width == 2 {
                                Cell {
                                    wide: CellWide::SpacerTail,
                                    ..Cell::default()
                                }
                            } else {
                                Cell::default()
                            };
                        }
                        let next = x + new_width as usize;
                        s.x = next.min(cols - 1);
                        s.wrap_pending = next >= cols;
                    }
                    self.last = Some((x, y, cp, new_width));
                    return;
                }
            }
        }
        self.grapheme = crate::unicode::State::default();
        if width == 0 {
            width = 1;
        }
        let cell = Cell {
            scalar: [cp],
            cluster: 0,
            style: self.style_id,
            wide: CellWide::Narrow,
            link: self.link_id,
        };
        let auto = self.mode(7);
        let insert = self.mode(0x8004);
        let style = self.erase_style_id;
        if !self
            .screen_mut()
            .put(cell, width as usize, auto, insert, style)
        {
            self.last = None;
            return;
        }
        let s = self.screen();
        let x = if s.wrap_pending {
            s.cols.saturating_sub(width as usize)
        } else {
            s.x.saturating_sub(width as usize)
        };
        self.last = Some((x, s.y, cp, width));
    }
    fn execute(&mut self, byte: u8) {
        match byte {
            7 => self.bells = self.bells.saturating_add(1),
            8 => {
                self.screen_mut().x = self.screen().x.saturating_sub(1);
                self.moved();
            }
            9 => {
                let x = self.screen().x;
                let next = self
                    .tabs
                    .range(x + 1..)
                    .next()
                    .copied()
                    .unwrap_or(self.screen().cols - 1);
                self.screen_mut().x = next.min(self.screen().cols - 1);
                self.moved();
            }
            10..=12 => {
                let style = self.erase_style_id;
                self.screen_mut().newline(style);
                if self.mode(0x8014) {
                    self.screen_mut().x = 0;
                }
                self.moved();
            }
            13 => {
                self.screen_mut().x = 0;
                self.moved();
            }
            14 => self.charset = 1,
            15 => self.charset = 0,
            _ => {}
        }
    }
    fn esc_dispatch(&mut self, inter: &[u8], ignore: bool, byte: u8) {
        if let Some(pending) = self.pending_osc.take() {
            if byte == b'\\' && inter.is_empty() && !ignore {
                let params: Vec<&[u8]> = pending.iter().map(|p| p.as_slice()).collect();
                self.dispatch_osc(&params, false);
            }
        }
        if ignore {
            return;
        }
        if inter == b"(" || inter == b")" {
            self.charset = if byte == b'0' { 1 } else { 0 };
            return;
        }
        if !inter.is_empty() {
            return;
        }
        match byte {
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'D' => self.execute(10),
            b'E' => {
                self.execute(13);
                self.execute(10);
            }
            b'M' => {
                if self.screen().y == self.screen().top {
                    let style = self.erase_style_id;
                    self.screen_mut().scroll_down(1, style);
                } else {
                    self.screen_mut().y = self.screen().y.saturating_sub(1);
                }
                self.moved();
            }
            b'H' => {
                self.tabs.insert(self.screen().x);
            }
            b'=' => {
                self.modes.insert(66, true);
            }
            b'>' => {
                self.modes.insert(66, false);
            }
            b'c' => {
                let cols = self.primary.cols;
                let rows = self.primary.rows;
                let budget = self.primary.budget;
                self.primary = Screen::new(cols, rows, budget);
                self.alternate = Screen::new(cols, rows, 0);
                self.alt = false;
                self.style = CellStyle::default();
                self.link = None;
                self.link_id = 0;
                self.links = vec![Arc::from("")];
                self.link_cache.clear();
                self.link_free.clear();
                self.link_bytes = 0;
                self.styles = Arc::new(vec![CellStyle::default()]);
                self.style_cache = HashMap::from([(CellStyle::default(), 0)]);
                self.style_free.clear();
                self.recent_style = None;
                self.style_id = 0;
                self.erase_style_id = 0;
                self.fg = None;
                self.bg = None;
                self.cursor_color = None;
                self.palette = self.defaults;
                self.keyboard = [vec![0], vec![0]];
                self.other_keys = false;
                self.reset_modes();
                self.last = None;
                self.graphics.reset();
                self.inactive_graphics.reset();
            }
            _ => {}
        }
    }
    #[inline]
    fn csi_dispatch(&mut self, params: &vte::Params, inter: &[u8], ignore: bool, action: char) {
        self.pending_osc = None;
        if ignore {
            return;
        }
        if inter.is_empty() {
            match action {
                'm' => {
                    self.sgr(params);
                    return;
                }
                'H' | 'f' => {
                    let mut params = params.iter();
                    let row = params.next().map_or(1, |p| p[0].max(1)) as usize;
                    let col = params.next().map_or(1, |p| p[0].max(1)) as usize;
                    let origin = self.mode(6);
                    let screen = self.screen_mut();
                    let top = if origin { screen.top } else { 0 };
                    let bottom = if origin {
                        screen.bottom
                    } else {
                        screen.rows - 1
                    };
                    screen.y = (top + row - 1).min(bottom);
                    screen.x = (col - 1).min(screen.cols - 1);
                    self.moved();
                    return;
                }
                _ => {}
            }
        }
        self.csi_dispatch_general(params, inter, ignore, action);
    }
    fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
        self.pending_osc = None;
        if bell {
            self.dispatch_osc(params, true);
        } else {
            self.pending_osc = Some(params.iter().map(|p| p.to_vec()).collect());
        }
    }
    fn hook(&mut self, _params: &vte::Params, inter: &[u8], _ignore: bool, action: char) {
        self.dcs.clear();
        self.dcs_kind = if inter == b"+" && action == 'q' {
            Some('q')
        } else if inter == b"$" && action == 'q' {
            Some('s')
        } else {
            None
        };
    }
    fn put(&mut self, byte: u8) {
        if self.dcs.len() < 8192 {
            self.dcs.push(byte);
        }
    }
    fn unhook(&mut self) {
        let kind = self.dcs_kind.take();
        if kind == Some('s') {
            let report = match self.dcs.as_slice() {
                b"m" => Some(
                    crate::render::style_ansi(self.style)
                        .trim_start_matches("\x1b[")
                        .to_owned(),
                ),
                b"r" => Some(format!(
                    "{};{}r",
                    self.screen().top + 1,
                    self.screen().bottom + 1
                )),
                b" q" => Some(format!(
                    "{} q",
                    match self.cursor_style {
                        CursorVisualStyle::Block | CursorVisualStyle::BlockHollow => 2,
                        CursorVisualStyle::Underline => 4,
                        CursorVisualStyle::Bar => 6,
                    } - u8::from(self.blinking)
                )),
                _ => None,
            };
            if let Some(report) = report {
                self.reply(format!("\x1bP1$r{report}\x1b\\").as_bytes());
            } else {
                self.reply(b"\x1bP0$r\x1b\\");
            }
        } else if kind == Some('q') {
            let data = std::mem::take(&mut self.dcs);
            for name in data.split(|b| *b == b';') {
                let Some(decoded) = unhex(name) else {
                    continue;
                };
                let value: Option<Option<&[u8]>> = match decoded.as_slice() {
                    b"Co" | b"colors" => Some(Some(b"256")),
                    b"TN" | b"name" => Some(Some(TERM.as_bytes())),
                    b"RGB" => Some(Some(b"8")),
                    b"setrgbf" => Some(Some(b"\\E[38:2:%p1%d:%p2%d:%p3%dm")),
                    b"setrgbb" => Some(Some(b"\\E[48:2:%p1%d:%p2%d:%p3%dm")),
                    b"Tc" | b"Su" => Some(None),
                    b"Ms" => Some(Some(b"\\E]52;%p1%s;%p2%s\\007")),
                    b"Smulx" => Some(Some(b"\\E[4:%p1%dm")),
                    b"Setulc" => Some(Some(
                        b"\\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m",
                    )),
                    _ => None,
                };
                if let Some(value) = value {
                    let mut out = Vec::from(b"\x1bP1+r".as_slice());
                    out.extend_from_slice(hex(&decoded).as_bytes());
                    if let Some(value) = value {
                        out.push(b'=');
                        out.extend_from_slice(hex(value).as_bytes());
                    }
                    out.extend_from_slice(b"\x1b\\");
                    self.reply(&out);
                }
            }
        }
    }
}
impl Terminal {
    fn color_reply(&mut self, command: &str, color: RgbColor, bell: bool) {
        self.reply(
            format!(
                "\x1b]{command};rgb:{:04x}/{:04x}/{:04x}{}",
                u16::from(color.r) * 257,
                u16::from(color.g) * 257,
                u16::from(color.b) * 257,
                if bell { "\x07" } else { "\x1b\\" }
            )
            .as_bytes(),
        );
    }
}
fn parse_color(bytes: &[u8]) -> Option<RgbColor> {
    let value = std::str::from_utf8(bytes).ok()?;
    if !value.is_ascii() {
        return None;
    }
    let parts: Vec<&str> = if let Some(v) = value.strip_prefix("rgb:") {
        v.split('/').collect()
    } else {
        let v = value.strip_prefix('#').unwrap_or(value);
        if !matches!(v.len(), 3 | 6 | 9 | 12) {
            return match v {
                "black" => Some(RgbColor::default()),
                "white" => Some(RgbColor {
                    r: 255,
                    g: 255,
                    b: 255,
                }),
                "red" => Some(RgbColor { r: 255, g: 0, b: 0 }),
                _ => None,
            };
        }
        let n = v.len() / 3;
        vec![&v[..n], &v[n..2 * n], &v[2 * n..]]
    };
    if parts.len() != 3 {
        return None;
    }
    let component = |p: &str| -> Option<u8> {
        if p.is_empty() || p.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(p, 16).ok()?;
        Some((v * 255 / ((1u32 << (p.len() * 4)) - 1)) as u8)
    };
    Some(RgbColor {
        r: component(parts[0])?,
        g: component(parts[1])?,
        b: component(parts[2])?,
    })
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}
fn unhex(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    bytes
        .chunks_exact(2)
        .map(|c| {
            std::str::from_utf8(c)
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
        })
        .collect()
}

impl Terminal {
    fn dispatch_osc(&mut self, params: &[&[u8]], bell: bool) {
        let Some(command) = params
            .first()
            .and_then(|p| std::str::from_utf8(p).ok())
            .and_then(|p| p.parse::<u16>().ok())
        else {
            return;
        };
        match command {
            7 => {
                if let Some(p) = params.get(1) {
                    self.pwd.push(p[..p.len().min(4096)].to_vec());
                }
            }
            9 => {
                if params.get(1) == Some(&b"9".as_slice()) {
                    if let Some(path) = params.get(2) {
                        self.pwd.push(path.to_vec());
                    }
                }
            }
            1337 => {
                if let Some(path) = params.get(1).and_then(|p| p.strip_prefix(b"CurrentDir=")) {
                    self.pwd.push(path.to_vec());
                }
            }
            8 => {
                let uri = params.get(2..).map(|v| v.join(&b';')).unwrap_or_default();
                self.link = if uri.is_empty() {
                    None
                } else {
                    Some(Arc::from(String::from_utf8_lossy(&uri).as_ref()))
                };
                self.sync_link();
            }
            52 => {
                if params.get(1).is_some_and(|p| p.is_empty() || *p == b"c") {
                    if let Some(data) = params.get(2).filter(|d| d.len() <= 256 * 1024) {
                        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                            if !bytes.is_empty() && bytes.len() <= 192 * 1024 {
                                self.clipboard.push(bytes);
                            }
                        }
                    }
                }
            }
            4 => {
                let mut response = String::from("\x1b]4");
                let mut queried = false;
                for pair in params[1..].chunks_exact(2) {
                    if let Some(index) = std::str::from_utf8(pair[0])
                        .ok()
                        .and_then(|v| v.parse::<usize>().ok())
                        .filter(|i| *i < 256)
                    {
                        if pair[1] == b"?" {
                            queried = true;
                            let color = self.palette[index];
                            response.push_str(&format!(
                                ";{index};rgb:{:04x}/{:04x}/{:04x}",
                                u16::from(color.r) * 257,
                                u16::from(color.g) * 257,
                                u16::from(color.b) * 257
                            ));
                        } else if let Some(color) = parse_color(pair[1]) {
                            self.palette[index] = color;
                        }
                    }
                }
                if queried {
                    response.push_str("\x1b\\");
                    self.reply(response.as_bytes());
                }
            }
            10..=12 => {
                for (offset, p) in params[1..].iter().enumerate() {
                    let command = command + offset as u16;
                    if command > 12 {
                        break;
                    }
                    if *p == b"?" {
                        let color = match command {
                            10 => self.fg.unwrap_or(RgbColor {
                                r: 255,
                                g: 255,
                                b: 255,
                            }),
                            11 => self.bg.unwrap_or_default(),
                            _ => self.cursor_color.or(self.fg).unwrap_or(RgbColor {
                                r: 255,
                                g: 255,
                                b: 255,
                            }),
                        };
                        self.color_reply(&command.to_string(), color, bell);
                    } else if let Some(color) = parse_color(p) {
                        match command {
                            10 => self.fg = Some(color),
                            11 => self.bg = Some(color),
                            _ => self.cursor_color = Some(color),
                        }
                    }
                }
            }
            104 => {
                if params.len() == 1 {
                    self.palette = self.defaults;
                } else {
                    for p in &params[1..] {
                        if let Some(i) = std::str::from_utf8(p)
                            .ok()
                            .and_then(|p| p.parse::<usize>().ok())
                            .filter(|i| *i < 256)
                        {
                            self.palette[i] = self.defaults[i];
                        }
                    }
                }
            }
            110 => self.fg = None,
            111 => self.bg = None,
            112 => self.cursor_color = None,
            _ => {}
        }
    }
}

impl Terminal {
    pub(crate) fn erase_style(&self) -> u16 {
        self.erase_style_id
    }
    pub(crate) fn cell_style(&self, cell: &Cell) -> CellStyle {
        self.styles
            .get(cell.style as usize)
            .copied()
            .unwrap_or_default()
    }
    fn cell_link(&self, cell: &Cell) -> Option<&Arc<str>> {
        if cell.link == 0 {
            None
        } else {
            self.links.get(cell.link as usize)
        }
    }
    fn intern_style(&mut self, style: CellStyle) -> u16 {
        if style == CellStyle::default() {
            return 0;
        }
        if let Some((previous, id)) = self.recent_style {
            if previous == style {
                return id;
            }
        }
        if let Some(id) = self.style_cache.get(&style).copied() {
            self.recent_style = Some((style, id));
            return id;
        }
        if self.styles.len() >= 65_536 && self.style_free.is_empty() {
            let mut used = vec![false; self.styles.len()];
            used[0] = true;
            used[self.style_id as usize] = true;
            used[self.erase_style_id as usize] = true;
            for screen in [&self.primary, &self.alternate] {
                for row in &screen.lines {
                    for cell in &row.cells {
                        used[cell.style as usize] = true;
                    }
                }
            }
            self.recent_style = None;
            self.style_cache.retain(|_, id| used[*id as usize]);
            self.style_free = (1..used.len())
                .filter(|id| !used[*id])
                .map(|id| id as u16)
                .collect();
        }
        let id = if let Some(id) = self.style_free.pop() {
            Arc::make_mut(&mut self.styles)[id as usize] = style;
            id
        } else if self.styles.len() < 65_536 {
            let id = self.styles.len() as u16;
            Arc::make_mut(&mut self.styles).push(style);
            id
        } else {
            return 0;
        };
        self.recent_style = Some((style, id));
        self.style_cache.insert(style, id);
        id
    }
    fn sync_style(&mut self) {
        self.style_id = self.intern_style(self.style);
        self.erase_style_id = if self.style.bg_color.is_none() {
            0
        } else {
            self.intern_style(CellStyle {
                bg_color: self.style.bg_color,
                ..CellStyle::default()
            })
        };
    }
    fn sync_link(&mut self) {
        let Some(link) = self.link.clone().filter(|link| link.len() <= 8192) else {
            self.link = None;
            self.link_id = 0;
            return;
        };
        if let Some(id) = self.link_cache.get(&link) {
            self.link_id = *id;
            return;
        }
        if (self.links.len() >= 65_536 && self.link_free.is_empty())
            || self.link_bytes + link.len() > 4 * 1024 * 1024
        {
            let mut used = vec![false; self.links.len()];
            used[0] = true;
            for screen in [&self.primary, &self.alternate] {
                for row in &screen.lines {
                    for cell in &row.cells {
                        used[cell.link as usize] = true;
                    }
                }
            }
            self.link_cache.retain(|_, id| used[*id as usize]);
            self.link_free = (1..used.len())
                .filter(|id| !used[*id])
                .map(|id| id as u16)
                .collect();
            for id in &self.link_free {
                self.links[*id as usize] = Arc::from("");
            }
            self.link_bytes = self.links.iter().map(|s| s.len()).sum();
        }
        if self.link_bytes + link.len() > 4 * 1024 * 1024 {
            self.link = None;
            self.link_id = 0;
            return;
        }
        let id = if let Some(id) = self.link_free.pop() {
            self.links[id as usize] = link.clone();
            id
        } else if self.links.len() < 65_536 {
            let id = self.links.len() as u16;
            self.links.push(link.clone());
            id
        } else {
            self.link = None;
            self.link_id = 0;
            return;
        };
        self.link_bytes += link.len();
        self.link_cache.insert(link, id);
        self.link_id = id;
    }
}

impl Terminal {
    fn csi_dispatch_general(
        &mut self,
        params: &vte::Params,
        inter: &[u8],
        ignore: bool,
        action: char,
    ) {
        self.pending_osc = None;
        if ignore {
            return;
        }
        if action == 'm' && inter.is_empty() {
            self.sgr(params);
            return;
        }
        let grouped: smallvec::SmallVec<[u16; 16]>;
        let p = if let Some(p) = params.as_single_params() {
            p
        } else {
            grouped = params.iter().map(|p| p[0]).collect();
            &grouped
        };
        let n = p.first().copied().unwrap_or(0);
        let count = n.max(1) as usize;
        if action == 'u' && matches!(inter, b">" | b"<" | b"=" | b"?") {
            let i = self.alt as usize;
            match inter {
                b">" => {
                    let value = (n & 31) as u8;
                    if self.keyboard[i].len() < 32 {
                        self.keyboard[i].push(value);
                    }
                }
                b"<" => {
                    for _ in 0..count.min(32) {
                        if self.keyboard[i].len() > 1 {
                            self.keyboard[i].pop();
                        }
                    }
                }
                b"=" => {
                    let current = self.keyboard[i].last_mut();
                    if let Some(current) = current {
                        let flags = n as u8 & 31;
                        *current = match p.get(1).copied().unwrap_or(1) {
                            2 => *current | flags,
                            3 => *current & !flags,
                            _ => flags,
                        };
                    }
                }
                b"?" => self.reply(
                    format!("\x1b[?{}u", self.kitty_keyboard_flags().unwrap_or(0)).as_bytes(),
                ),
                _ => {}
            }
            return;
        }
        if action == 'n' && inter == b">" && n == 4 {
            self.other_keys = false;
            return;
        }
        if action == 'm' && inter == b">" {
            if n == 4 {
                self.other_keys = p.get(1) == Some(&2);
            }
            return;
        }
        if action == 'm' && inter == b"?" {
            if n == 4 {
                self.reply(format!("\x1b[>4;{}m", if self.other_keys { 2 } else { 0 }).as_bytes());
            }
            return;
        }
        if matches!(action, 'h' | 'l') {
            if params.into_iter().any(|v| v.len() != 1) {
                return;
            }
            for mode in p.iter().copied() {
                let mode = if inter == b"?" { mode } else { mode | 0x8000 };
                self.set_mode(mode, action == 'h');
            }
            return;
        }
        if inter == b"?" && matches!(action, 's' | 'r') {
            for mode in p.iter().copied() {
                if action == 's' {
                    self.saved_modes.insert(mode, self.mode(mode));
                } else {
                    let value = self.saved_modes.get(&mode).copied().unwrap_or(false);
                    self.set_mode(mode, value);
                }
            }
            return;
        }
        if action == 'p' && inter == b"?$" {
            let value = if self.mode(n) { 1 } else { 2 };
            self.reply(format!("\x1b[?{n};{value}$y").as_bytes());
            return;
        }
        let style = self.erase_style_id;
        match action {
            'm' if inter.is_empty() => self.sgr(params),
            'A' | 'B' | 'C' | 'D' | 'E' | 'F' | 'G' | 'H' | 'f' | 'd' | '`' | 'a' | 'e' => {
                let origin = self.mode(6);
                let s = self.screen_mut();
                let top = if origin { s.top } else { 0 };
                let bottom = if origin { s.bottom } else { s.rows - 1 };
                match action {
                    'A' | 'F' => s.y = s.y.saturating_sub(count).max(top),
                    'B' | 'E' | 'e' => s.y = (s.y + count).min(bottom),
                    'C' | 'a' => s.x = (s.x + count).min(s.cols - 1),
                    'D' => s.x = s.x.saturating_sub(count),
                    'G' | '`' => s.x = (count - 1).min(s.cols - 1),
                    'd' => s.y = (top + count - 1).min(bottom),
                    'H' | 'f' => {
                        s.y = (top + count - 1).min(bottom);
                        s.x = (p.get(1).copied().unwrap_or(1).max(1) as usize - 1).min(s.cols - 1);
                    }
                    _ => {}
                }
                if matches!(action, 'E' | 'F') {
                    s.x = 0;
                }
                self.moved();
            }
            'J' => {
                if n == 2 {
                    self.flush_graphics_scrolls();
                    let screen = if self.alt {
                        &self.alternate
                    } else {
                        &self.primary
                    };
                    self.graphics.clear_screen(screen, self.cell_height);
                }
                let s = self.screen_mut();
                let (x, y, cols, rows) = (s.x, s.y, s.cols, s.rows);
                match n {
                    0 => {
                        s.erase(y, x, cols, style);
                        for row in y + 1..rows {
                            s.erase(row, 0, cols, style);
                        }
                    }
                    1 => {
                        for row in 0..y {
                            s.erase(row, 0, cols, style);
                        }
                        s.erase(y, 0, x + 1, style);
                    }
                    2 => s.clear(style),
                    3 => s.clear_history(),
                    _ => {}
                }
                self.last = None;
            }
            'K' => {
                let s = self.screen_mut();
                match n {
                    0 => s.erase(s.y, s.x, s.cols, style),
                    1 => s.erase(s.y, 0, s.x + 1, style),
                    2 => s.erase(s.y, 0, s.cols, style),
                    _ => {}
                }
                self.last = None;
            }
            'X' => {
                let s = self.screen_mut();
                s.erase(s.y, s.x, (s.x + count).min(s.cols), style);
                self.last = None;
            }
            '@' | 'P' => {
                let s = self.screen_mut();
                let (x, y, cols) = (s.x, s.y, s.cols);
                let len = count.min(cols - x);
                let row = s.live_row_mut(y);
                if action == '@' {
                    row.cells[x..].rotate_right(len);
                    row.cells[x..x + len].fill(Cell::blank(style));
                } else {
                    row.cells[x..].rotate_left(len);
                    row.cells[cols - len..].fill(Cell::blank(style));
                }
                row.repair_wide();
                self.moved();
            }
            'L' | 'M' => {
                let s = self.screen_mut();
                if s.y >= s.top && s.y <= s.bottom {
                    let top = s.top;
                    let scrolls = s.scrolls.len();
                    s.top = s.y;
                    if action == 'L' {
                        s.scroll_down(count, style);
                    } else {
                        s.delete_lines(count, style);
                    }
                    s.scrolls.truncate(scrolls);
                    s.top = top;
                }
                self.moved();
            }
            'S' | 'T' => {
                if action == 'S' {
                    self.screen_mut().scroll_up(count, style);
                } else {
                    self.screen_mut().scroll_down(count, style);
                }
                // Scrolling may move or remove the glyph tracked by `last`.
                self.last = None;
                self.grapheme = crate::unicode::State::default();
            }
            'r' if inter.is_empty() => {
                let origin = self.mode(6);
                let s = self.screen_mut();
                let top = (count - 1).min(s.rows - 1);
                let bottom = p
                    .get(1)
                    .copied()
                    .filter(|n| *n > 0)
                    .map_or(s.rows - 1, |n| (n as usize - 1).min(s.rows - 1));
                if top < bottom {
                    s.top = top;
                    s.bottom = bottom;
                    s.x = 0;
                    s.y = if origin { top } else { 0 };
                }
                self.moved();
            }
            's' if inter.is_empty() => self.save_cursor(),
            'u' if inter.is_empty() => self.restore_cursor(),
            'g' => {
                if n == 3 {
                    self.tabs.clear();
                } else if n == 0 {
                    let x = self.screen().x;
                    self.tabs.remove(&x);
                }
            }
            'I' | 'Z' => {
                for _ in 0..count.min(self.screen().cols) {
                    let x = self.screen().x;
                    self.screen_mut().x = if action == 'I' {
                        self.tabs
                            .range(x + 1..)
                            .next()
                            .copied()
                            .unwrap_or(self.screen().cols - 1)
                    } else {
                        self.tabs.range(..x).next_back().copied().unwrap_or(0)
                    };
                }
                self.moved();
            }
            'q' if inter == b" " => {
                self.cursor_style = match n {
                    3 | 4 => CursorVisualStyle::Underline,
                    5 | 6 => CursorVisualStyle::Bar,
                    _ => CursorVisualStyle::Block,
                };
                self.blinking = n == 0 || n % 2 == 1;
            }
            'n' => match (inter, n) {
                (b"", 5) => self.reply(b"\x1b[0n"),
                (b"", 6) => self.reply(
                    format!("\x1b[{};{}R", self.screen().y + 1, self.screen().x + 1).as_bytes(),
                ),
                (b"?", 996) => self.report_scheme(),
                _ => {}
            },
            'c' => {
                if inter.is_empty() {
                    self.reply(b"\x1b[?62;22c");
                } else if inter == b">" {
                    self.reply(b"\x1b[>1;0;0c");
                }
            }
            't' => {
                if self.cell_width > 0 && self.cell_height > 0 {
                    match n {
                        14 => self.reply(
                            format!(
                                "\x1b[4;{};{}t",
                                self.height_px().unwrap_or(0),
                                self.width_px().unwrap_or(0)
                            )
                            .as_bytes(),
                        ),
                        16 => self.reply(
                            format!("\x1b[6;{};{}t", self.cell_height, self.cell_width).as_bytes(),
                        ),
                        18 => self.reply(
                            format!("\x1b[8;{};{}t", self.screen().rows, self.screen().cols)
                                .as_bytes(),
                        ),
                        _ => {}
                    }
                }
            }
            'b' => {
                if let Some((_, _, cp, _)) = self.last {
                    if let Some(c) = char::from_u32(cp) {
                        for _ in 0..count.min(65535) {
                            self.print(c);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(any(test, feature = "fuzzing"))]
#[path = "fuzz_support.rs"]
pub mod fuzz_support;
