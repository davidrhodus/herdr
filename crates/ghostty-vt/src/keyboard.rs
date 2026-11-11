//! Rust implementation of legacy, modifyOtherKeys and Kitty keyboard encoding.
use crate::{ffi, Error, Terminal, MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

#[path = "keymap.rs"]
mod keymap;

struct FunctionEntry {
    mods: u16,
    any_mods: bool,
    cursor: u8,
    keypad: u8,
    other: u8,
    sequence: &'static [u8],
    backarrow: Option<&'static [u8]>,
}

pub struct KeyEvent {
    action: ffi::GhosttyKeyAction,
    key: ffi::GhosttyKey,
    mods: u16,
    consumed: u16,
    text: String,
    unshifted: u32,
    composing: bool,
}

impl KeyEvent {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            action: ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS,
            key: 0,
            mods: 0,
            consumed: 0,
            text: String::new(),
            unshifted: 0,
            composing: false,
        })
    }
    pub fn set_action(&mut self, action: ffi::GhosttyKeyAction) {
        self.action = action;
    }
    pub fn set_key(&mut self, key: ffi::GhosttyKey) {
        self.key = key;
    }
    pub fn set_mods(&mut self, mods: u16) {
        self.mods = mods;
    }
    pub fn set_utf8(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
    }
    pub fn set_unshifted_codepoint(&mut self, codepoint: u32) {
        self.unshifted = codepoint;
    }
    pub fn set_consumed_mods(&mut self, mods: u16) {
        self.consumed = mods;
    }
    pub fn set_composing(&mut self, composing: bool) {
        self.composing = composing;
    }
    fn binding_mods(&self) -> u16 {
        self.mods
            & if self.text.is_empty() {
                u16::MAX
            } else {
                !self.consumed
            }
            & 15
    }
}

pub struct KeyEncoder {
    cursor: bool,
    keypad: bool,
    backarrow: bool,
    ignore_numlock: bool,
    alt_prefix: bool,
    other: bool,
    kitty: u8,
    option_as_alt: bool,
}

fn sequence_mods(mods: u16) -> u16 {
    (mods & MOD_SHIFT) | ((mods & MOD_ALT) >> 1) | ((mods & MOD_CTRL) << 1) | (mods & MOD_SUPER)
}

fn control(cp: u32) -> bool {
    cp < 32 || cp == 127
}
fn control_text(text: &str) -> bool {
    text.len() == 1 && control(u32::from(text.as_bytes()[0]))
}
fn single(text: &str) -> Option<u32> {
    let mut chars = text.chars();
    let cp = chars.next()? as u32;
    chars.next().is_none().then_some(cp)
}

impl KeyEncoder {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            cursor: false,
            keypad: false,
            backarrow: false,
            ignore_numlock: false,
            alt_prefix: false,
            other: false,
            kitty: 0,
            option_as_alt: false,
        })
    }
    pub fn set_from_terminal(&mut self, terminal: &Terminal) {
        self.cursor = terminal.mode_get(1).unwrap_or(false);
        self.keypad = terminal.mode_get(66).unwrap_or(false);
        self.backarrow = terminal.mode_get(67).unwrap_or(false);
        self.ignore_numlock = terminal.mode_get(1035).unwrap_or(false);
        self.alt_prefix = terminal.mode_get(1036).unwrap_or(false);
        self.other = terminal.modify_other_keys_enabled().unwrap_or(false);
        self.kitty = terminal.kitty_keyboard_flags().unwrap_or(0);
        self.option_as_alt = false;
    }
    pub fn set_macos_option_as_alt(&mut self, enabled: bool) {
        self.option_as_alt = enabled;
    }
    pub fn encode(&mut self, event: &KeyEvent) -> Result<Vec<u8>, Error> {
        Ok(if self.kitty == 0 {
            self.legacy(event)
        } else {
            self.kitty(event)
        })
    }

    fn function_key(&self, event: &KeyEvent) -> Option<&'static [u8]> {
        let keypad = self.keypad && !self.ignore_numlock;
        for entry in keymap::function_entries(event.key) {
            if (entry.cursor == 1 && self.cursor)
                || (entry.cursor == 2 && !self.cursor)
                || (entry.keypad == 1 && keypad)
                || (entry.keypad == 2 && !keypad)
                || (entry.other == 1 && self.other)
                || (entry.other == 2 && !self.other)
                || (entry.mods != event.mods & 15 && !(entry.mods == 0 && entry.any_mods))
            {
                continue;
            }
            return Some(if self.backarrow {
                entry.backarrow.unwrap_or(entry.sequence)
            } else {
                entry.sequence
            });
        }
        None
    }

    fn control_sequence(event: &KeyEvent) -> Option<u8> {
        if event.mods & MOD_CTRL == 0 {
            return None;
        }
        let mut mods = event.mods & 15 & !MOD_ALT;
        let mut cp = if event.text.len() == 1 {
            u32::from(event.text.as_bytes()[0])
        } else {
            if mods != MOD_CTRL {
                return None;
            }
            keymap::codepoint(event.key).filter(|cp| *cp <= 255)?
        };
        if mods & MOD_SHIFT != 0 && !(65..=90).contains(&cp) && cp != 64 {
            mods &= !MOD_SHIFT;
        }
        if (65..=90).contains(&cp) && (1..=255).contains(&event.unshifted) {
            cp = event.unshifted;
        }
        if mods != MOD_CTRL {
            return None;
        }
        Some(match cp {
            32 | 50 | 64 => 0,
            47 | 55 | 95 => 31,
            48 | 49 | 57 => cp as u8,
            51 => 27,
            52 | 92 => 28,
            53 | 93 => 29,
            54 | 94 | 126 => 30,
            56 | 63 => 127,
            97..=122 if cp != 105 && cp != 109 => cp as u8 - 96,
            _ => return None,
        })
    }

    fn alt_text(&self, event: &KeyEvent) -> Option<Vec<u8>> {
        if event.binding_mods() & MOD_ALT == 0
            || !self.alt_prefix
            || (cfg!(target_os = "macos") && !self.option_as_alt)
        {
            return None;
        }
        let mut result = vec![27];
        if event.text.len() == 1 {
            result.extend_from_slice(event.text.as_bytes());
        } else if event.unshifted > 0 && (cfg!(target_os = "macos") || event.text.is_empty()) {
            let cp = char::from_u32(event.unshifted)?;
            result.extend_from_slice(cp.encode_utf8(&mut [0; 4]).as_bytes());
        } else if !event.text.is_empty() {
            result.extend_from_slice(event.text.as_bytes());
        } else {
            return None;
        }
        Some(result)
    }

    fn legacy(&self, event: &KeyEvent) -> Vec<u8> {
        if event.action == ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_RELEASE || event.composing {
            return Vec::new();
        }
        if let Some(sequence) = self.function_key(event) {
            let ime = !event.text.is_empty()
                && !control_text(&event.text)
                && matches!(
                    event.key,
                    ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE
                        | ffi::GhosttyKey_GHOSTTY_KEY_ENTER
                        | ffi::GhosttyKey_GHOSTTY_KEY_ESCAPE
                );
            if ime {
                if event.key == ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE {
                    return Vec::new();
                }
            } else {
                return sequence.to_vec();
            }
        }
        if self.other {
            if let Some(cp) = single(&event.text) {
                let mut mods = event.mods & 15;
                if cfg!(target_os = "macos") && !self.option_as_alt {
                    mods &= !MOD_ALT;
                }
                if mods != 0 && ((64..=127).contains(&cp) || mods & !MOD_SHIFT != 0 || cp == 32) {
                    return format!("\x1b[27;{};{cp}~", sequence_mods(mods) + 1).into_bytes();
                }
            }
        }
        if let Some(byte) = Self::control_sequence(event) {
            return if event.binding_mods() & MOD_ALT != 0 {
                vec![27, byte]
            } else {
                vec![byte]
            };
        }
        if event.text.is_empty() {
            return self.alt_text(event).unwrap_or_default();
        }
        if event.mods & MOD_CTRL != 0 {
            if let Some(mut cp) = single(&event.text) {
                let mut mods = event.mods & 7;
                if (65..=90).contains(&cp) && mods & MOD_SHIFT != 0 {
                    cp += 32;
                }
                if event.unshifted != cp {
                    mods &= !MOD_SHIFT;
                }
                return format!("\x1b[{cp};{}u", sequence_mods(mods) + 1).into_bytes();
            }
        }
        if let Some(result) = self.alt_text(event) {
            return result;
        }
        if cfg!(target_os = "macos") && event.mods & MOD_SUPER != 0 {
            return Vec::new();
        }
        event.text.as_bytes().to_vec()
    }

    fn kitty(&self, event: &KeyEvent) -> Vec<u8> {
        use ffi::{
            GhosttyKeyAction_GHOSTTY_KEY_ACTION_RELEASE as RELEASE,
            GhosttyKey_GHOSTTY_KEY_BACKSPACE as BACKSPACE, GhosttyKey_GHOSTTY_KEY_ENTER as ENTER,
            GhosttyKey_GHOSTTY_KEY_TAB as TAB,
        };
        let release = event.action == RELEASE;
        let all = self.kitty & 8 != 0;
        let events = self.kitty & 2 != 0;
        let special = matches!(event.key, ENTER | BACKSPACE | TAB);
        if release && (!events || (!all && special)) {
            return Vec::new();
        }
        let entry = keymap::kitty_entry(event.key)
            .or_else(|| (event.unshifted > 0).then_some((event.unshifted, 'u', false)));
        if event.composing && !entry.is_some_and(|entry| entry.2) {
            return Vec::new();
        }
        if !event.composing {
            if !event.text.is_empty() && !control_text(&event.text) {
                if event.key == BACKSPACE {
                    return Vec::new();
                }
                if event.key == ENTER {
                    return event.text.as_bytes().to_vec();
                }
            }
            if !all && event.binding_mods() == 0 {
                if special {
                    return vec![match event.key {
                        ENTER => 13,
                        TAB => 9,
                        _ => 127,
                    }];
                }
                if !release
                    && !event.text.is_empty()
                    && event.text.chars().all(|cp| !control(cp as u32))
                {
                    return event.text.as_bytes().to_vec();
                }
            }
        }
        let Some((code, final_char, modifier)) = entry else {
            return if release {
                Vec::new()
            } else {
                event.text.as_bytes().to_vec()
            };
        };
        if modifier && !all {
            return Vec::new();
        }
        let mods = sequence_mods(event.mods & 15)
            + ((event.mods & 16) << 2)
            + ((event.mods & 32) << 2)
            + 1;
        let event_code = if !events {
            0
        } else {
            match event.action {
                0 => 3,
                2 => 2,
                _ => 1,
            }
        };
        if final_char != 'u' && final_char != '~' {
            return if event_code > 0 {
                format!("\x1b[1;{mods}:{event_code}{final_char}")
            } else if mods > 1 {
                format!("\x1b[1;{mods}{final_char}")
            } else {
                format!("\x1b[{final_char}")
            }
            .into_bytes();
        }
        let mut sequence = format!("\x1b[{code}");
        if self.kitty & 4 != 0 && !control(code) {
            let shifted = event
                .text
                .chars()
                .next()
                .map(|cp| cp as u32)
                .filter(|cp| *cp != code && event.mods & MOD_SHIFT != 0);
            let base = keymap::codepoint(event.key).filter(|base| {
                *base != code
                    && (event.text.is_empty() || single(&event.text).is_some_and(|cp| cp != *base))
            });
            if let Some(shifted) = shifted {
                sequence.push_str(&format!(":{shifted}"));
            }
            if let Some(base) = base {
                sequence.push_str(if shifted.is_none() { "::" } else { ":" });
                sequence.push_str(&base.to_string());
            }
        }
        let prior = event_code > 1 || mods > 1;
        if event_code > 1 {
            sequence.push_str(&format!(";{mods}:{event_code}"));
        } else if mods > 1 {
            sequence.push_str(&format!(";{mods}"));
        }
        let alt_prevents = !cfg!(target_os = "macos") || self.option_as_alt;
        if self.kitty & 16 != 0
            && !release
            && event.mods & (MOD_CTRL | MOD_SUPER) == 0
            && !(alt_prevents && event.mods & MOD_ALT != 0)
        {
            let mut first = true;
            for cp in event
                .text
                .chars()
                .map(|cp| cp as u32)
                .filter(|cp| !control(*cp))
            {
                if first {
                    sequence.push_str(if prior { ";" } else { ";;" });
                    first = false;
                } else {
                    sequence.push(':');
                }
                sequence.push_str(&cp.to_string());
            }
        }
        sequence.push(final_char);
        sequence.into_bytes()
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    // Golden SHA-256 streams captured from libghostty-vt 44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51.
    // Keyboard digests include macOS Option/Command behavior; app conformance tests
    // separately exercise the platform-independent keyboard contracts on every OS.
    #[test]
    #[cfg(target_os = "macos")]
    fn native_keyboard_matches_reference_across_protocols() {
        crate::fixtures::reset();

        for flags in 0..=31 {
            for options in [0, 1, 2, 3, 4, 8, 16, 32, 63] {
                let mut terminal = Terminal::new(80, 24, 0).unwrap();
                for (bit, mode) in [(1, 1), (2, 66), (4, 67), (8, 1035), (16, 1036)] {
                    terminal.mode_set(mode, options & bit != 0).unwrap();
                }
                if options & 32 != 0 {
                    terminal.write(b"\x1b[>4;2m");
                }
                terminal.write(format!("\x1b[>{flags}u").as_bytes());
                let mut native = KeyEncoder::new().unwrap();
                native.set_from_terminal(&terminal);
                native.set_macos_option_as_alt(true);
                for key in 0..=175 {
                    for mods in 0..16 {
                        for action in 0..=2 {
                            let mut event = KeyEvent::new().unwrap();
                            event.key = key;
                            event.mods = mods;
                            event.action = action;
                            if let Some(cp) = keymap::codepoint(key) {
                                event.unshifted = cp;
                                if let Some(character) = char::from_u32(cp) {
                                    event.text = if mods & MOD_SHIFT != 0 {
                                        character.to_uppercase().collect()
                                    } else {
                                        character.to_string()
                                    };
                                }
                            }
                            crate::fixtures::observe(&native.encode(&event).unwrap());
                        }
                    }
                }
            }
        }
        crate::fixtures::check("a971549c5fa29a49e4af355202d655150e2286d50f94c4d0d687f6bf3f030806");
    }
    #[test]
    #[cfg(target_os = "macos")]
    fn native_keyboard_matches_reference_for_layout_and_ime_text() {
        crate::fixtures::reset();

        for flags in [0, 1, 2, 4, 8, 16, 31] {
            for option_as_alt in [false, true] {
                let mut terminal = Terminal::new(80, 24, 0).unwrap();
                terminal.write(format!("\x1b[>{flags}u").as_bytes());
                let mut native = KeyEncoder::new().unwrap();
                native.set_from_terminal(&terminal);
                native.set_macos_option_as_alt(option_as_alt);
                for (key, cp) in [
                    (0, 0),
                    (crate::KEY_A, 97),
                    (crate::KEY_A, 0x444),
                    (crate::KEY_ENTER, 0),
                    (ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE, 0),
                    (ffi::GhosttyKey_GHOSTTY_KEY_ESCAPE, 27),
                ] {
                    for text in [
                        "",
                        "a",
                        "A",
                        "é",
                        "Ö",
                        "!",
                        " ",
                        "\r",
                        "\x7f",
                        "文",
                        "👩‍💻",
                        "composed",
                    ] {
                        for mods in 0..64 {
                            for consumed in [0, MOD_SHIFT, MOD_ALT] {
                                for composing in [false, true] {
                                    for action in 0..=2 {
                                        let event = KeyEvent {
                                            key,
                                            unshifted: cp,
                                            text: text.into(),
                                            mods,
                                            consumed,
                                            composing,
                                            action,
                                        };
                                        crate::fixtures::observe(&native.encode(&event).unwrap());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        crate::fixtures::check("7d40819ea5165e6337168c2d061a704b90b9aa1412ac374451597427df37d157");
    }
}
