//! Native Rust mouse protocol encoding. Events and encoder state are ordinary
//! Rust values; no foreign encoder handles are used.
use crate::{ffi, Error, Terminal, MOD_ALT, MOD_CTRL, MOD_SHIFT};

// Mouse reporting selects the most recently requested mode, rather than the
// highest numbered enabled DEC bit. Keep that state while parsing the stream.
// The terminal parser updates this state directly; mouse input never reparses output.
#[derive(Default)]
pub(crate) struct Modes {
    event: u16,
    format: ffi::GhosttyMouseFormat,
    bits: [bool; 8],
}

impl Modes {
    pub(crate) fn active(&self) -> bool {
        self.event != 0
    }
    fn index(mode: u16) -> Option<usize> {
        [9, 1000, 1002, 1003, 1005, 1006, 1015, 1016]
            .iter()
            .position(|value| *value == mode)
    }
    pub(crate) fn set_bit(&mut self, mode: u16, enabled: bool) {
        if let Some(index) = Self::index(mode) {
            self.bits[index] = enabled;
        }
    }
    pub(crate) fn set(&mut self, mode: u16, enabled: bool) {
        self.set_bit(mode, enabled);
        match mode {
            9 | 1000 | 1002 | 1003 => self.event = if enabled { mode } else { 0 },
            1005 => self.format = if enabled { 1 } else { 0 },
            1006 => self.format = if enabled { 2 } else { 0 },
            1015 => self.format = if enabled { 3 } else { 0 },
            1016 => self.format = if enabled { 4 } else { 0 },
            _ => {}
        }
    }
}

#[derive(Clone, Copy)]
pub struct MouseEvent {
    action: ffi::GhosttyMouseAction,
    button: Option<ffi::GhosttyMouseButton>,
    mods: u16,
    x: f32,
    y: f32,
}

impl MouseEvent {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            action: 0,
            button: None,
            mods: 0,
            x: 0.0,
            y: 0.0,
        })
    }
    pub fn set_action(&mut self, action: ffi::GhosttyMouseAction) {
        self.action = action;
    }
    pub fn set_button(&mut self, button: ffi::GhosttyMouseButton) {
        self.button = Some(button);
    }
    pub fn clear_button(&mut self) {
        self.button = None;
    }
    pub fn set_mods(&mut self, mods: u16) {
        self.mods = mods;
    }
    pub fn set_position(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
    }
}

pub struct MouseEncoder {
    mode: u16,
    format: ffi::GhosttyMouseFormat,
    width: u32,
    height: u32,
    cell_width: u32,
    cell_height: u32,
}

impl MouseEncoder {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            mode: 0,
            format: 0,
            width: 1,
            height: 1,
            cell_width: 1,
            cell_height: 1,
        })
    }

    pub fn set_from_terminal(&mut self, terminal: &Terminal) {
        self.mode = terminal.mouse_modes.event;
        self.format = terminal.mouse_modes.format;
    }

    pub fn set_size(&mut self, width: u32, height: u32, cell_width: u32, cell_height: u32) {
        if cell_width == 0 || cell_height == 0 {
            return;
        }
        self.width = width;
        self.height = height;
        self.cell_width = cell_width;
        self.cell_height = cell_height;
    }

    pub fn set_format(&mut self, format: ffi::GhosttyMouseFormat) {
        if (0..=4).contains(&format) {
            self.format = format;
        }
    }

    pub fn encode(&mut self, event: &MouseEvent) -> Result<Vec<u8>, Error> {
        let press = event.action == 0;
        let release = event.action == 1;
        let motion = event.action == 2;
        let report = match self.mode {
            9 => press && matches!(event.button, Some(1..=3)),
            1000 => !motion,
            1002 => event.button.is_some(),
            1003 => true,
            _ => false,
        };
        if !report || !(press || release || motion) || !event.x.is_finite() || !event.y.is_finite()
        {
            return Ok(Vec::new());
        }
        // The facade does not enable the encoder's any-button-pressed option,
        // so only releases may be reported outside the viewport.
        if !release
            && (event.x < 0.0
                || event.y < 0.0
                || event.x > self.width as f32
                || event.y > self.height as f32)
        {
            return Ok(Vec::new());
        }
        let sgr = self.format == 2 || self.format == 4;
        let mut code = if event.button.is_none() || (release && !sgr) {
            3
        } else {
            match event.button {
                Some(1) => 0,
                Some(2) => 2,
                Some(3) => 1,
                Some(4..=7) => 64 + event.button.unwrap_or(4) as u8 - 4,
                Some(8..=9) => 128 + event.button.unwrap_or(8) as u8 - 8,
                _ => return Ok(Vec::new()),
            }
        };
        if self.mode != 9 {
            if event.mods & MOD_SHIFT != 0 {
                code += 4;
            }
            if event.mods & MOD_ALT != 0 {
                code += 8;
            }
            if event.mods & MOD_CTRL != 0 {
                code += 16;
            }
        }
        if motion {
            code += 32;
        }
        let col = (event.x.max(0.0) / self.cell_width as f32).floor() as u32;
        let row = (event.y.max(0.0) / self.cell_height as f32).floor() as u32;
        let col = col.min((self.width / self.cell_width).saturating_sub(1));
        let row = row.min((self.height / self.cell_height).saturating_sub(1));
        let suffix = if release { 'm' } else { 'M' };
        Ok(match self.format {
            0 if col > 222 || row > 222 => Vec::new(),
            0 => vec![
                27,
                b'[',
                b'M',
                32 + code,
                (33 + col) as u8,
                (33 + row) as u8,
            ],
            1 => {
                let mut result = vec![27, b'[', b'M', 32 + code];
                for coordinate in [col, row] {
                    let Some(character) = char::from_u32(coordinate.saturating_add(33)) else {
                        return Ok(Vec::new());
                    };
                    result.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
                }
                result
            }
            2 => format!("\x1b[<{code};{};{}{suffix}", col + 1, row + 1).into_bytes(),
            3 => format!("\x1b[{};{};{}M", 32 + u16::from(code), col + 1, row + 1).into_bytes(),
            4 => format!(
                "\x1b[<{code};{};{}{suffix}",
                event.x.round() as i32,
                event.y.round() as i32
            )
            .into_bytes(),
            _ => Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Golden SHA-256 streams captured from libghostty-vt 44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51.
    // Keyboard digests include macOS Option/Command behavior; app conformance tests
    // separately exercise the platform-independent keyboard contracts on every OS.
    #[test]
    fn native_mouse_matches_reference_across_protocols() {
        crate::fixtures::reset();

        for mode in [0, 9, 1000, 1002, 1003] {
            let mut terminal = Terminal::new(80, 24, 0).unwrap();
            if mode != 0 {
                terminal.write(format!("\x1b[?{mode}h").as_bytes());
            }
            for format in 0..=4 {
                let mut native = MouseEncoder::new().unwrap();
                native.set_from_terminal(&terminal);
                native.set_format(format);
                native.set_size(640, 384, 8, 16);
                for action in 0..=2 {
                    for button in [
                        None,
                        Some(1),
                        Some(2),
                        Some(3),
                        Some(4),
                        Some(7),
                        Some(8),
                        Some(9),
                        Some(10),
                    ] {
                        for mods in 0..16 {
                            for (x, y) in [
                                (0.0, 0.0),
                                (8.0, 16.0),
                                (15.4, 31.6),
                                (640.0, 384.0),
                                (-1.0, 0.0),
                                (641.0, 385.0),
                            ] {
                                let event = MouseEvent {
                                    action,
                                    button,
                                    mods,
                                    x,
                                    y,
                                };
                                crate::fixtures::observe(&native.encode(&event).unwrap());
                            }
                        }
                    }
                }
            }
        }
        crate::fixtures::check("1101bb91e0c5991aee6a9b2ebfe5173443a285e32eee431eaa93302a21895514");
    }
    #[test]
    fn native_mouse_mode_transitions_match_reference_at_every_byte_boundary() {
        crate::fixtures::reset();

        let streams: &[&[u8]] = &[
            b"\x1b[?1003h\x1b[?1000h\x1b[?1002l",
            b"\x1b[?1006h\x1b[?1016h\x1b[?1005h\x1b[?1006l",
            b"\x1b[?1000;1006h\x1b[?1000;1006s\x1b[?1002;1016h\x1b[?1000;1006r",
            b"\x1b[?1003;1016h\x1bc",
            b"\x1b[?1003;1016h\x1b[!p",
            b"\x1b]0;ignored \x1b[?1000h\x07",
            b"\x1bPignored \x1b[?1000h\x1b\\",
            b"\x1b_ignored \x1b[?1000h\x1b\\",
            b"\x1b[?1000\x18h\x1b[?1002h",
            b"\x1b[?1003;1006h\x1b[?1000:2h",
            b"\x1b[?1003;1006h\x1b[?1000;1002:2h",
            b"\x1b[?1003;1006h\x1b[?1000;;1016h",
            b"\x9b?1003;1006h",
            b"\xc2\x9b?1003;1006h",
            b"\x1b[?1003;1006h\x1b[?65536;1000h",
        ];
        for &stream in streams {
            for split in 0..=stream.len() {
                let mut terminal = Terminal::new(80, 24, 0).unwrap();
                terminal.write(&stream[..split]);
                terminal.write(&stream[split..]);
                let mut native = MouseEncoder::new().unwrap();
                native.set_from_terminal(&terminal);
                native.set_size(640, 384, 8, 16);
                for action in 0..=2 {
                    for button in [None, Some(1), Some(3), Some(4)] {
                        let event = MouseEvent {
                            action,
                            button,
                            mods: 0,
                            x: 10.0,
                            y: 20.0,
                        };
                        crate::fixtures::observe(&native.encode(&event).unwrap());
                    }
                }
            }
        }
        crate::fixtures::check("840b6c692a39307f9073d15bbb50b5958e9ef135d8603ed7203f10eb33c69b47");
    }
}
