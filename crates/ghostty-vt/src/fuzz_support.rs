//! Oracles shared by deterministic smoke tests and coverage-guided fuzz targets.
//! Compiled only for tests or the opt-in `fuzzing` feature. Raw input keeps
//! graphics disabled, so arbitrary bytes cannot request filesystem media.
use super::*;
use std::sync::Mutex;

const MAX_INPUT: usize = 8192;
const COMPLETIONS: &[&[u8]] = &[
    b"mX",
    b"hX",
    b";?\x07",
    b"\x80\x81\xacX",
    b"\x1b\\",
    b"\\X",
    "\u{301}\u{fe0f}\u{200d}🟦".as_bytes(),
];
struct Runner {
    terminal: Terminal,
    snapshot: RenderState,
    replies: Arc<Mutex<Vec<u8>>>,
}
impl Runner {
    fn new(cols: u16, rows: u16, history: usize) -> Self {
        let mut terminal = Terminal::new(cols, rows, history).expect("bounded fuzz dimensions");
        let replies = Arc::new(Mutex::new(Vec::new()));
        let sink = replies.clone();
        terminal
            .set_write_pty_callback(move |bytes| {
                sink.lock().expect("reply sink").extend_from_slice(bytes)
            })
            .expect("callback");
        let mut snapshot = RenderState::new().expect("snapshot");
        snapshot.update(&terminal).expect("snapshot");
        Self {
            terminal,
            snapshot,
            replies,
        }
    }
    fn feed(&mut self, bytes: &[u8]) {
        self.terminal.write(bytes);
        self.check_invariants();
        self.snapshot
            .update(&self.terminal)
            .expect("snapshot update");
        self.snapshot.clean().expect("render cycle clean");
    }
    fn fragmented(&mut self, bytes: &[u8], mut schedule: u64, bytewise: bool) {
        let mut rest = bytes;
        self.feed(&[]);
        while !rest.is_empty() {
            schedule = random(schedule);
            let n = if bytewise {
                1
            } else {
                1 + (schedule as usize % 31)
            };
            let n = n.min(rest.len());
            self.feed(&rest[..n]);
            rest = &rest[n..];
        }
        self.feed(&[]);
    }
    fn check_invariants(&self) {
        for screen in [&self.terminal.primary, &self.terminal.alternate] {
            assert!(
                screen.x < screen.cols && screen.y < screen.rows,
                "cursor outside screen"
            );
            assert!(
                screen.top <= screen.bottom && screen.bottom < screen.rows,
                "invalid margins"
            );
            assert!(screen.lines.len() >= screen.rows && screen.offset <= screen.history());
            if screen.budget == 0 {
                assert_eq!(screen.history(), 0);
            }
            let bytes: usize = screen
                .lines
                .iter()
                .take(screen.history())
                .map(|row| row.bytes())
                .sum();
            assert_eq!(bytes, screen.history_bytes, "history accounting drift");
            for row in &screen.lines {
                assert!(row.cells.len() <= screen.cols);
                for (x, cell) in row.cells.iter().enumerate() {
                    if cell.wide == CellWide::Wide {
                        assert_eq!(
                            row.cell(x + 1).wide,
                            CellWide::SpacerTail,
                            "orphan wide head at {x}"
                        );
                    }
                    if cell.wide == CellWide::SpacerTail {
                        assert!(
                            x > 0 && row.cell(x - 1).wide == CellWide::Wide,
                            "orphan wide tail at {x}"
                        );
                    }
                    assert!((cell.style as usize) < self.terminal.styles.len());
                    assert!((cell.link as usize) < self.terminal.links.len());
                    if cell.cluster != 0 {
                        assert!(row
                            .graphemes
                            .get(cell.cluster as usize - 1)
                            .is_some_and(Option::is_some));
                    }
                    let text = row.text(cell);
                    assert!(text.len() <= 256);
                    assert!(text
                        .iter()
                        .all(|cp| *cp != 0 && char::from_u32(*cp).is_some()));
                }
            }
        }
        if let Some((x, y, cp, _)) = self.terminal.last {
            let screen = self.terminal.screen();
            assert!(
                x < screen.cols && y < screen.rows,
                "grapheme anchor outside screen"
            );
            let row = &screen.lines[screen.history() + y];
            assert_eq!(
                row.text(row.cell(x)).last(),
                Some(&cp),
                "stale grapheme anchor"
            );
        }
        for mode in [6, 7, 2027, 0x8004] {
            assert_eq!(
                self.terminal.mode(mode),
                self.terminal.modes.get(&mode).copied().unwrap_or(false),
                "cached mode {mode}"
            );
        }
    }
}
fn assert_runners(a: &Runner, b: &Runner) {
    let (left, right) = (&a.terminal, &b.terminal);
    for (index, (a, b)) in [&left.primary, &left.alternate]
        .iter()
        .zip([&right.primary, &right.alternate])
        .enumerate()
    {
        assert_eq!(
            (a.cols, a.rows, a.x, a.y, a.wrap_pending),
            (b.cols, b.rows, b.x, b.y, b.wrap_pending),
            "screen {index} geometry/cursor"
        );
        assert_eq!(
            (a.top, a.bottom, a.offset, a.dropped),
            (b.top, b.bottom, b.offset, b.dropped),
            "screen {index} scroll state"
        );
        assert_eq!(
            (
                a.saved.x,
                a.saved.y,
                a.saved.style,
                a.saved.origin,
                a.saved.wrap_pending
            ),
            (
                b.saved.x,
                b.saved.y,
                b.saved.style,
                b.saved.origin,
                b.saved.wrap_pending
            ),
            "screen {index} saved cursor"
        );
        assert_eq!(a.lines.len(), b.lines.len(), "screen {index} rows");
        let cols = a.cols;
        for (y, (a, b)) in a.lines.iter().zip(&b.lines).enumerate() {
            assert_eq!(
                (a.wrapped, a.continuation),
                (b.wrapped, b.continuation),
                "screen {index} row {y} wrapping"
            );
            for x in 0..cols {
                let (ca, cb) = (a.cell(x), b.cell(x));
                assert_eq!(
                    (ca.wide, a.text(ca)),
                    (cb.wide, b.text(cb)),
                    "screen {index} cell {x},{y}"
                );
                assert_eq!(
                    left.styles[ca.style as usize], right.styles[cb.style as usize],
                    "cell style"
                );
                assert_eq!(
                    left.links.get(ca.link as usize),
                    right.links.get(cb.link as usize),
                    "cell link"
                );
            }
        }
    }
    let mut ia = RowIterator::new().expect("rows");
    let mut ib = RowIterator::new().expect("rows");
    let mut ra = a.snapshot.populate_row_iterator(&mut ia).expect("rows");
    let mut rb = b.snapshot.populate_row_iterator(&mut ib).expect("rows");
    let mut sa = RowCells::new().expect("cells");
    let mut sb = RowCells::new().expect("cells");
    let mut ta = String::with_capacity(32);
    let mut tb = String::with_capacity(32);
    let mut bytes = Vec::new();
    loop {
        let next = ra.next();
        assert_eq!(next, rb.next(), "render rows");
        if !next {
            break;
        }
        let mut ca = ra.populate_cells(&mut sa).expect("cells");
        let mut cb = rb.populate_cells(&mut sb).expect("cells");
        loop {
            let next = ca.next();
            assert_eq!(next, cb.next(), "render columns");
            if !next {
                break;
            }
            assert_eq!(
                ca.basic_data().expect("cell"),
                cb.basic_data().expect("cell")
            );
            ca.grapheme_text_into(&mut bytes, &mut ta).expect("text");
            cb.grapheme_text_into(&mut bytes, &mut tb).expect("text");
            assert_eq!(ta, tb, "render text");
        }
    }
    assert_eq!(
        left.active_screen().expect("screen"),
        right.active_screen().expect("screen")
    );
    assert_eq!(
        a.snapshot.cursor().expect("cursor"),
        b.snapshot.cursor().expect("cursor")
    );
    assert_eq!(
        a.snapshot.colors().expect("colors"),
        b.snapshot.colors().expect("colors")
    );
    let compare_modes = |a: &HashMap<u16, bool>, b: &HashMap<u16, bool>| {
        for (mode, value) in a.iter().chain(b.iter()) {
            if *value {
                assert_eq!(
                    a.get(mode).copied().unwrap_or(false),
                    b.get(mode).copied().unwrap_or(false),
                    "mode {mode}"
                );
            }
        }
    };
    compare_modes(&left.modes, &right.modes);
    compare_modes(&left.saved_modes, &right.saved_modes);
    assert_eq!(left.tabs, right.tabs);
    assert_eq!(left.keyboard, right.keyboard);
    assert_eq!(
        (left.other_keys, left.charset, left.style),
        (right.other_keys, right.charset, right.style)
    );
    assert_eq!(
        (left.fg, left.bg, left.cursor_color),
        (right.fg, right.bg, right.cursor_color)
    );
    assert_eq!(left.defaults, right.defaults);
    assert_eq!(left.palette, right.palette);
    assert_eq!(left.link, right.link);
    assert_eq!(
        *a.replies.lock().expect("replies"),
        *b.replies.lock().expect("replies")
    );
    assert_eq!(left.pwd, right.pwd);
    assert_eq!(left.clipboard, right.clipboard);
    assert_eq!(left.bells, right.bells);
}
fn random(mut state: u64) -> u64 {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    state
}
fn settings(data: &[u8]) -> (u16, u16, u64, &[u8]) {
    let cols = 1 + u16::from(data.first().copied().unwrap_or(7) % 24);
    let rows = 1 + u16::from(data.get(1).copied().unwrap_or(3) % 8);
    let schedule = 1 + u64::from(data.get(2).copied().unwrap_or(0));
    (cols, rows, schedule, data.get(3..).unwrap_or_default())
}
/// Compare one write with randomized fragmentation, including bytewise feeds
/// for small inputs and a continuation probe for incomplete parser states.
pub fn check_chunking(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let (cols, rows, schedule, bytes) = settings(data);
    let mut whole = Runner::new(cols, rows, 0);
    whole.feed(bytes);
    for bytewise in [false, true] {
        if bytewise && bytes.len() > 256 {
            continue;
        }
        let mut split = Runner::new(cols, rows, 0);
        split.fragmented(bytes, schedule, bytewise);
        assert_runners(&whole, &split);
        let completion = COMPLETIONS[schedule as usize % COMPLETIONS.len()];
        const PROBE: &[u8] = b"\x1b\\\x18\x1b[0m\x1b[Hok\x1b[6n";
        let mut reference = Runner::new(cols, rows, 0);
        reference.feed(bytes);
        reference.feed(completion);
        split.feed(completion);
        assert_runners(&reference, &split);
        reference.feed(PROBE);
        split.feed(PROBE);
        assert_runners(&reference, &split);
    }
}
/// Exercise the same bounded write/resize/scroll/compress operations with
/// different PTY fragmentation and a retained render snapshot on each side.
pub fn check_operations(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let (cols, rows, seed, mut input) = settings(data);
    let mut whole = Runner::new(cols, rows, 128 * 1024);
    let mut split = Runner::new(cols, rows, 128 * 1024);
    for step in 0..64 {
        let Some((&op, rest)) = input.split_first() else {
            break;
        };
        input = rest;
        match op % 6 {
            0 | 1 => {
                let Some((&length, rest)) = input.split_first() else {
                    break;
                };
                let n = (length as usize % 65).min(rest.len());
                input = &rest[n..];
                whole.feed(&rest[..n]);
                split.fragmented(&rest[..n], seed + step, false);
            }
            2 => {
                if input.len() < 2 {
                    break;
                }
                let cols = 1 + u16::from(input[0] % 24);
                let rows = 1 + u16::from(input[1] % 8);
                input = &input[2..];
                whole.terminal.resize(cols, rows, 8, 16).expect("resize");
                split.terminal.resize(cols, rows, 8, 16).expect("resize");
            }
            3 => {
                assert_eq!(whole.terminal.clear_screen(), split.terminal.clear_screen());
            }
            4 => {
                let Some((&delta, rest)) = input.split_first() else {
                    break;
                };
                input = rest;
                whole.terminal.scroll_viewport_delta(delta as i8 as isize);
                split.terminal.scroll_viewport_delta(delta as i8 as isize);
            }
            _ => {
                whole.terminal.compress_incremental().expect("compress");
                split.terminal.compress_incremental().expect("compress");
            }
        }
        whole.feed(&[]);
        split.feed(&[]);
        assert_runners(&whole, &split);
    }
    let completion = COMPLETIONS[seed as usize % COMPLETIONS.len()];
    whole.feed(completion);
    split.feed(completion);
    assert_runners(&whole, &split);
}

#[cfg(test)]
mod tests {
    use super::*;
    const TOKENS: &[&[u8]] = &[
        b"text",
        "界".as_bytes(),
        "e\u{301}".as_bytes(),
        "♥\u{fe0f}".as_bytes(),
        "👩\u{200d}💻".as_bytes(),
        b"\r\n",
        b"\x1b[H",
        b"\x1b[0m",
        b"\x1b[31m",
        b"\x1b[38:2::17:34:51m",
        b"\x1b[2J",
        b"\x1b[K",
        b"\x1b[1X",
        b"\x1b[P",
        b"\x1b[@",
        b"\x1b[L",
        b"\x1b[M",
        b"\x1b[4h",
        b"\x1b[4l",
        b"\x1b[?6h",
        b"\x1b[?6l",
        b"\x1b[?7l",
        b"\x1b[?7h",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b[2;3r",
        b"\x1b7",
        b"\x1b8",
        b"\x1b]8;;https://example.test\x1b\\",
        b"\x1b]8;;\x07",
        b"\x1b]7;file:///tmp/herdr\x1b\\",
        b"\x1bP$qm\x1b\\",
        b"\x1b_Gcancel\x18",
        b"\xff\xc2",
    ];
    #[test]
    fn parser_fuzz_regression_wide_growth_clears_overlapping_glyph() {
        check_chunking(include_bytes!(
            "../../../fuzz/corpus/parser_chunking/seed-regression-wide-growth"
        ));
    }
    #[test]
    fn parser_fuzz_regression_clear_keeps_wrapped_cursor_in_viewport() {
        check_operations(include_bytes!(
            "../../../fuzz/corpus/parser_operations/seed-regression-clear-wrapped"
        ));
    }
    #[test]
    fn parser_fuzz_regression_wide_wrap_and_insert_keep_paired_cells() {
        check_chunking(include_bytes!(
            "../../../fuzz/corpus/parser_chunking/seed-regression-wide-wrap"
        ));
        check_chunking(include_bytes!(
            "../../../fuzz/corpus/parser_chunking/seed-regression-wide-insert"
        ));
    }
    #[test]
    fn parser_fuzz_regression_skipped_wide_glyph_cannot_own_a_cluster() {
        check_chunking(include_bytes!(
            "../../../fuzz/corpus/parser_chunking/seed-regression-skipped-wide"
        ));
    }
    #[test]
    fn parser_fuzz_regression_skipped_and_scrolled_glyphs_clear_the_anchor() {
        check_chunking(include_bytes!(
            "../../../fuzz/corpus/parser_chunking/seed-regression-nowrap-wide"
        ));
        check_chunking(include_bytes!(
            "../../../fuzz/corpus/parser_chunking/seed-regression-scroll-anchor"
        ));
    }
    #[test]
    fn parser_fuzz_seed_corpus_is_chunk_invariant() {
        let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus");
        for (name, check) in [
            ("parser_chunking", check_chunking as fn(&[u8])),
            ("parser_operations", check_operations as fn(&[u8])),
        ] {
            let mut seeds: Vec<_> = std::fs::read_dir(corpus.join(name))
                .expect("seed corpus")
                .map(|p| p.expect("seed entry").path())
                .filter(|p| {
                    p.file_name()
                        .expect("file name")
                        .to_string_lossy()
                        .starts_with("seed-")
                })
                .collect();
            seeds.sort();
            assert!(!seeds.is_empty());
            for seed in seeds {
                let bytes = std::fs::read(&seed).expect("seed");
                check(&bytes);
            }
        }
    }
    #[test]
    fn parser_fuzz_structured_smoke() {
        for seed in 1..=512u64 {
            let mut state = seed;
            let mut data = vec![
                (seed % 24) as u8,
                ((seed >> 3) % 8) as u8,
                (seed % 255) as u8,
            ];
            for _ in 0..16 {
                state = random(state);
                data.extend_from_slice(TOKENS[state as usize % TOKENS.len()]);
            }
            if std::panic::catch_unwind(|| check_chunking(&data)).is_err() {
                panic!("structured seed {seed}: {data:02x?}");
            }
        }
    }
    #[test]
    fn parser_fuzz_structured_operations_smoke() {
        for seed in 1..=256u64 {
            let mut state = seed;
            let mut data = vec![
                (seed % 24) as u8,
                ((seed >> 3) % 8) as u8,
                (seed % 255) as u8,
            ];
            for n in 0..16 {
                state = random(state);
                let token = TOKENS[state as usize % TOKENS.len()];
                data.extend_from_slice(&[0, token.len() as u8]);
                data.extend_from_slice(token);
                if n % 3 == 0 {
                    data.extend_from_slice(&[2, (state >> 8) as u8, (state >> 16) as u8]);
                }
                if n % 5 == 0 {
                    data.extend_from_slice(&[3, 5]);
                }
            }
            if std::panic::catch_unwind(|| check_operations(&data)).is_err() {
                panic!("operation seed {seed}: {data:02x?}");
            }
        }
    }
    #[test]
    fn parser_fuzz_arbitrary_bytes_smoke() {
        for seed in 1..=256u64 {
            let mut state = seed;
            let mut data = Vec::new();
            for _ in 0..(3 + seed as usize % 96) {
                state = random(state);
                data.push(state as u8);
            }
            if std::panic::catch_unwind(|| check_chunking(&data)).is_err() {
                panic!("byte seed {seed}: {data:02x?}");
            }
        }
    }
}
