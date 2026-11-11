//! Terminal-specific Unicode widths and streaming grapheme boundaries.
//! Ported from Ghostty's Unicode kernel and uucode's tailored UAX #29 rules.
#[path = "unicode_tables.rs"]
mod tables;

#[derive(Clone, Copy)]
struct Properties {
    width: u8,
    zero_in_grapheme: bool,
    class: u8,
    emoji_vs_base: bool,
}

fn properties(cp: u32) -> Properties {
    if cp > 0x10ffff {
        return Properties {
            width: 1,
            zero_in_grapheme: false,
            class: 0,
            emoji_vs_base: false,
        };
    }
    let block = usize::from(tables::STAGE1[(cp >> 8) as usize]);
    tables::PROPERTIES[usize::from(tables::STAGE2[block + (cp & 255) as usize])]
}

pub fn unicode_codepoint_width(cp: u32) -> u8 {
    properties(cp).width
}

// The numeric classes are fixed by the checked-in Unicode property table.
const PREPEND: u8 = 1;
const REGIONAL: u8 = 2;
const SPACING: u8 = 3;
const L: u8 = 4;
const V: u8 = 5;
const T: u8 = 6;
const LV: u8 = 7;
const LVT: u8 = 8;
const ZWJ: u8 = 9;
const ZWNJ: u8 = 10;
const PICTOGRAPHIC: u8 = 11;
const MODIFIER_BASE: u8 = 12;
const MODIFIER: u8 = 13;
const INDIC_EXTEND: u8 = 14;
const INDIC_LINKER: u8 = 15;
const INDIC_CONSONANT: u8 = 16;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum State {
    #[default]
    Default,
    Regional,
    Pictographic,
    IndicConsonant,
    IndicLinker,
}

fn extend(class: u8) -> bool {
    matches!(class, ZWNJ | INDIC_EXTEND | INDIC_LINKER)
}
fn pictographic(class: u8) -> bool {
    matches!(class, PICTOGRAPHIC | MODIFIER_BASE)
}
fn indic_extend(class: u8) -> bool {
    matches!(class, INDIC_EXTEND | ZWJ)
}

fn grapheme_break(first: u8, second: u8, state: &mut State) -> bool {
    let valid = match *state {
        State::Default => true,
        State::Regional => first == REGIONAL && second == REGIONAL,
        State::Pictographic => [first, second].into_iter().all(|class| {
            matches!(
                class,
                INDIC_EXTEND | INDIC_LINKER | ZWNJ | ZWJ | PICTOGRAPHIC | MODIFIER_BASE | MODIFIER
            )
        }),
        State::IndicConsonant | State::IndicLinker => [first, second]
            .into_iter()
            .all(|class| matches!(class, INDIC_CONSONANT | INDIC_LINKER | INDIC_EXTEND | ZWJ)),
    };
    if !valid {
        *state = State::Default;
    }
    if (first == L && matches!(second, L | V | LV | LVT))
        || (matches!(first, LV | V) && matches!(second, V | T))
        || (matches!(first, LVT | T) && second == T)
        || second == SPACING
        || first == PREPEND
    {
        return false;
    }
    if first == INDIC_CONSONANT {
        if indic_extend(second) {
            *state = State::IndicConsonant;
            return false;
        }
        if second == INDIC_LINKER {
            *state = State::IndicLinker;
            return false;
        }
    } else if *state == State::IndicConsonant {
        if second == INDIC_LINKER {
            *state = State::IndicLinker;
            return false;
        }
        if indic_extend(second) {
            return false;
        }
        *state = State::Default;
    } else if *state == State::IndicLinker {
        if second == INDIC_LINKER || indic_extend(second) {
            return false;
        }
        *state = State::Default;
        if second == INDIC_CONSONANT {
            return false;
        }
    }
    if pictographic(first) {
        if extend(second) || second == ZWJ || (first == MODIFIER_BASE && second == MODIFIER) {
            *state = State::Pictographic;
            return false;
        }
    } else if *state == State::Pictographic {
        if (extend(first) || first == MODIFIER) && (extend(second) || second == ZWJ) {
            return false;
        }
        *state = State::Default;
        if first == ZWJ && pictographic(second) {
            return false;
        }
    }
    if first == REGIONAL && second == REGIONAL {
        if *state == State::Default {
            *state = State::Regional;
            return false;
        }
        *state = State::Default;
        return true;
    }
    !(extend(second) || second == ZWJ)
}

pub fn unicode_grapheme_width(codepoints: &[u32]) -> (usize, u8) {
    let Some(&first) = codepoints.first() else {
        return (0, 0);
    };
    if first > 0x10ffff {
        return (1, 1);
    }
    let mut width = properties(first).width;
    let mut previous = first;
    let mut state = State::Default;
    let mut consumed = 1;
    for &cp in &codepoints[1..] {
        if cp > 0x10ffff {
            break;
        }
        let prior_state = state;
        let props = properties(cp);
        if grapheme_break(properties(previous).class, props.class, &mut state) {
            break;
        }
        consumed += 1;
        if cp == 0xfe0f || cp == 0xfe0e {
            if !properties(previous).emoji_vs_base {
                state = prior_state;
                continue;
            }
            width = if cp == 0xfe0f { 2 } else { 1 };
        } else if !props.zero_in_grapheme {
            width = 2;
        }
        previous = cp;
    }
    (consumed, width)
}

pub(crate) fn append(previous: u32, cp: u32, state: &mut State, width: u8) -> Option<(u8, bool)> {
    let before = *state;
    let props = properties(cp);
    if grapheme_break(properties(previous).class, props.class, state) {
        return None;
    }
    if matches!(cp, 0xfe0e | 0xfe0f) {
        if !properties(previous).emoji_vs_base {
            *state = before;
            return Some((width, false));
        }
        return Some((if cp == 0xfe0f { 2 } else { 1 }, true));
    }
    Some((if props.zero_in_grapheme { width } else { 2 }, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Golden SHA-256 streams captured from libghostty-vt 44f2a44df7e8c4a0c6df3f7d872ef3d7ead88e51.
    // Keyboard digests include macOS Option/Command behavior; app conformance tests
    // separately exercise the platform-independent keyboard contracts on every OS.
    fn observe(cps: &[u32]) {
        let (n, width) = unicode_grapheme_width(cps);
        let mut bytes = (n as u32).to_le_bytes().to_vec();
        bytes.push(width);
        crate::fixtures::observe(&bytes);
    }
    #[test]
    fn native_codepoint_width_matches_reference_for_every_codepoint() {
        crate::fixtures::reset();

        for cp in 0..=0x10ffff {
            crate::fixtures::observe(&[unicode_codepoint_width(cp)]);
        }
        for cp in [0x110000, u32::MAX] {
            assert_eq!(unicode_codepoint_width(cp), 1);
        }
        crate::fixtures::check("c1331048d1d9dacc687f39d25e7a1ddb1b73d494c9db2e9b103aa984d6d5d442");
    }
    #[test]
    fn native_graphemes_match_reference_across_property_transitions() {
        crate::fixtures::reset();

        // Include representatives of every property tuple, selectors, invalid
        // scalars, and longer chains to exercise all state transitions.
        let mut representatives = Vec::new();
        let mut seen = [false; tables::PROPERTIES.len()];
        for cp in 0..=0x10ffff {
            let block = usize::from(tables::STAGE1[(cp >> 8) as usize]);
            let index = usize::from(tables::STAGE2[block + (cp & 255) as usize]);
            if !seen[index] {
                seen[index] = true;
                representatives.push(cp);
            }
        }
        representatives.extend([0xfe0e, 0xfe0f, 0x110000, u32::MAX]);
        observe(&[]);
        for &a in &representatives {
            for &b in &representatives {
                for &c in &representatives {
                    let cps = [a, b, c, a, b, c, b, c];
                    observe(&cps);
                }
            }
        }
        crate::fixtures::check("59506526333684c61d6a528ffad3f5f5ed2ef99e822f64efdf96909f3aee43ab");
    }
}
