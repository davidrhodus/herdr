//! Native Rust terminal engine for Herdr.

use std::{fmt, ops::RangeInclusive, sync::Arc};

use pane_graphics_files::OwnedExport;

// Preserve the existing public input constant names for application compatibility.
#[allow(non_camel_case_types, non_upper_case_globals)]
pub mod bindings;

pub use bindings as ffi;

pub mod pane_graphics_files;

#[cfg(target_os = "linux")]
mod native_image_sources;

mod keyboard;
pub use keyboard::{KeyEncoder, KeyEvent};

mod mouse;
pub use mouse::{MouseEncoder, MouseEvent};

mod unicode;
pub use unicode::{unicode_codepoint_width, unicode_grapheme_width};

mod graphics;
mod render;
mod screen;
mod terminal;

pub use render::*;
pub use terminal::Terminal;

pub const TERM: &str = "xterm-256color";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub(crate) i32);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "terminal error {}", self.0)
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirty {
    Clean,
    Partial,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCompressionResult {
    Unsupported,
    Pending,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSelection {
    pub start_x: u16,
    pub end_x: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEvent {
    Gained,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum KittyImageFormat {
    Rgb,
    Rgba,
    Png,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImagePlacement {
    pub image_id: u32,
    pub placement_id: u32,
    pub z: i32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub image_width: u32,
    pub image_height: u32,
    pub format: KittyImageFormat,
    pub data_len: usize,
    pub data_fingerprint: u64,
    pub data: Vec<u8>,
    pub source_file: Option<Arc<OwnedExport>>,
    pub render: KittyPlacementRenderInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyImageDescriptor {
    pub image_id: u32,
    pub placement_id: u32,
    pub image_width: u32,
    pub image_height: u32,
    pub format: KittyImageFormat,
    pub data_len: usize,
    pub data_fingerprint: u64,
    pub source_file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyPlacementRenderInfo {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub grid_cols: u32,
    pub grid_rows: u32,
    pub viewport_col: i32,
    pub viewport_row: i32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorVisualStyle {
    Bar,
    Block,
    Underline,
    BlockHollow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveScreen {
    Primary,
    Alternate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalScrollbar {
    pub total: usize,
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorViewport {
    pub x: u16,
    pub y: u16,
    pub wide_tail: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderCursor {
    pub viewport: Option<CursorViewport>,
    pub visible: bool,
    pub blinking: bool,
    pub visual_style: CursorVisualStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellColor {
    Palette(u8),
    Rgb(RgbColor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct CellStyle {
    pub fg_color: Option<CellColor>,
    pub bg_color: Option<CellColor>,
    pub underline_color: Option<CellColor>,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: u8,
    pub underlined: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderColors {
    pub background: RgbColor,
    pub foreground: RgbColor,
    pub palette: [RgbColor; 256],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CellWide {
    Narrow,
    Wide,
    SpacerTail,
    SpacerHead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenTextCell {
    pub wide: CellWide,
    pub graphemes: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenTextRow {
    pub cells: Vec<ScreenTextCell>,
    pub soft_wrapped: bool,
    pub wrap_continuation: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LinkTarget {
    Uri(String),
    Text { text: String, clicked_byte: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkRegion {
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellBasicData {
    pub wide: CellWide,
    pub has_hyperlink: bool,
    pub has_styling: bool,
    pub style: CellStyle,
}

pub const MOD_SHIFT: u16 = ffi::GHOSTTY_MODS_SHIFT as u16;
pub const MOD_CTRL: u16 = ffi::GHOSTTY_MODS_CTRL as u16;
pub const MOD_ALT: u16 = ffi::GHOSTTY_MODS_ALT as u16;
pub const MOD_SUPER: u16 = ffi::GHOSTTY_MODS_SUPER as u16;

pub const KEY_ENTER: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_ENTER;
pub const KEY_UP: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_UP;
pub const KEY_DOWN: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_DOWN;
pub const KEY_LEFT: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_LEFT;
pub const KEY_RIGHT: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_ARROW_RIGHT;
pub const KEY_A: ffi::GhosttyKey = ffi::GhosttyKey_GHOSTTY_KEY_A;

pub const MOUSE_ACTION_PRESS: ffi::GhosttyMouseAction =
    ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_PRESS;
pub const MOUSE_ACTION_RELEASE: ffi::GhosttyMouseAction =
    ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_RELEASE;
pub const MOUSE_ACTION_MOTION: ffi::GhosttyMouseAction =
    ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_MOTION;
pub const MOUSE_BUTTON_LEFT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_LEFT;
pub const MOUSE_BUTTON_RIGHT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_RIGHT;
pub const MOUSE_BUTTON_MIDDLE: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_MIDDLE;
pub const MOUSE_BUTTON_WHEEL_UP: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_FOUR;
pub const MOUSE_BUTTON_WHEEL_DOWN: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_FIVE;
pub const MOUSE_BUTTON_WHEEL_LEFT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_SIX;
pub const MOUSE_BUTTON_WHEEL_RIGHT: ffi::GhosttyMouseButton =
    ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_SEVEN;
pub const MOUSE_FORMAT_SGR: ffi::GhosttyMouseFormat =
    ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR;
pub const MOUSE_FORMAT_SGR_PIXELS: ffi::GhosttyMouseFormat =
    ffi::GhosttyMouseFormat_GHOSTTY_MOUSE_FORMAT_SGR_PIXELS;

pub const MODE_APPLICATION_CURSOR_KEYS: u16 = 1;
pub const MODE_FOCUS_EVENT: u16 = 1004;
pub const MODE_MOUSE_UTF8: u16 = 1005;
pub const MODE_MOUSE_SGR: u16 = 1006;
pub const MODE_MOUSE_ALTERNATE_SCROLL: u16 = 1007;
pub const MODE_MOUSE_SGR_PIXELS: u16 = 1016;
pub const MODE_BRACKETED_PASTE: u16 = 2004;
pub const MODE_SYNCHRONIZED_OUTPUT: u16 = 2026;
pub const MODE_GRAPHEME_CLUSTER: u16 = 2027;
pub const MODE_COLOR_SCHEME_REPORT: u16 = 2031;

pub const KITTY_UNICODE_PLACEHOLDER: u32 = 0x10EEEE;

impl RowSelection {
    pub fn range(self) -> RangeInclusive<u16> {
        self.start_x..=self.end_x
    }
}

impl Default for CellBasicData {
    fn default() -> Self {
        Self {
            wide: CellWide::Narrow,
            has_hyperlink: false,
            has_styling: false,
            style: CellStyle::default(),
        }
    }
}

pub fn encode_focus(event: FocusEvent) -> Result<Vec<u8>, Error> {
    Ok(match event {
        FocusEvent::Gained => b"\x1b[I".to_vec(),
        FocusEvent::Lost => b"\x1b[O".to_vec(),
    })
}

pub fn default_palette() -> [RgbColor; 256] {
    let mut p = [RgbColor::default(); 256];
    let named = [
        0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, 0x666666,
        0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
    ];
    for (i, n) in named.into_iter().enumerate() {
        p[i] = RgbColor {
            r: (n >> 16) as u8,
            g: (n >> 8) as u8,
            b: n as u8,
        };
    }
    for (i, color) in p.iter_mut().enumerate().take(232).skip(16) {
        let v = i - 16;
        let channel = |n: usize| if n == 0 { 0 } else { (55 + 40 * n) as u8 };
        *color = RgbColor {
            r: channel(v / 36),
            g: channel(v / 6 % 6),
            b: channel(v % 6),
        };
    }
    for (i, c) in p.iter_mut().enumerate().skip(232) {
        let n = (8 + (i - 232) * 10) as u8;
        *c = RgbColor { r: n, g: n, b: n };
    }
    p
}

#[cfg(test)]
mod test_allocations;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_source_tests;

#[cfg(test)]
mod png_forward_tests;

mod native_source;
pub(crate) struct FingerprintEntry {
    pub generation: u64,
    pub fingerprint: u64,
}
#[cfg(test)]
use graphics::PNG_DECODE_CALLS;

#[cfg(test)]
mod fixtures {
    use sha2::{Digest, Sha256};
    thread_local! {static HASH:std::cell::RefCell<Sha256>=std::cell::RefCell::new(Sha256::new());}
    pub fn reset() {
        HASH.with(|h| *h.borrow_mut() = Sha256::new());
    }
    pub fn observe(bytes: &[u8]) {
        HASH.with(|h| {
            let mut h = h.borrow_mut();
            h.update((bytes.len() as u32).to_le_bytes());
            h.update(bytes);
        });
    }
    pub fn check(expected: &str) {
        HASH.with(|h| assert_eq!(format!("{:x}", h.borrow().clone().finalize()), expected));
    }
}

#[cfg(test)]
mod parse_benchmark;

#[cfg(test)]
mod render_parse_benchmark;

#[cfg(test)]
mod render_snapshot_tests;
