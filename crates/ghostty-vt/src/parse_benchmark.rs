//! Fixed-work parser benchmark. Run with `just bench-parse-scale`.
use crate::{CellColor, RenderState, RgbColor, RowCells, RowIterator, Terminal};
use std::{hint::black_box, time::Instant};

fn stream(styled: bool) -> String {
    if styled {
        format!(
            "\x1b[H{}",
            (0..24)
                .map(|row| format!(
                    "\x1b[{};1H\x1b[38;2;17;34;51m{}\x1b[0m",
                    row + 1,
                    "text ".repeat(16)
                ))
                .collect::<String>()
        )
    } else {
        format!(
            "\x1b[H{}\x1b[?1000;1006h\x1b[?1000;1006l",
            "a".repeat(80 * 24)
        )
    }
}

fn check_output(terminal: &Terminal, styled: bool) {
    let text = terminal
        .read_text_viewport((0, 0), (79, 23), false)
        .expect("benchmark text");
    let expected = if styled {
        vec!["text ".repeat(16).trim_end().to_owned(); 24].join("\n")
    } else {
        "a".repeat(80 * 24)
    };
    assert_eq!(text, expected);
    let mut state = RenderState::new().expect("render state");
    state.update(terminal).expect("snapshot");
    let mut iterator = RowIterator::new().expect("row iterator");
    let mut rows = state.populate_row_iterator(&mut iterator).expect("rows");
    let mut storage = RowCells::new().expect("cell storage");
    let mut count = 0;
    while rows.next() {
        let mut cells = rows.populate_cells(&mut storage).expect("cells");
        while cells.next() {
            assert_eq!(
                cells.style().expect("style").fg_color,
                styled.then_some(CellColor::Rgb(RgbColor {
                    r: 17,
                    g: 34,
                    b: 51
                }))
            );
            count += 1;
        }
    }
    assert_eq!(count, 80 * 24);
}

#[test]
fn parser_scale_fixtures_preserve_all_text_and_foreground_colors() {
    for styled in [false, true] {
        let mut terminal = Terminal::new(80, 24, 0).expect("benchmark terminal");
        terminal.write(stream(styled).as_bytes());
        check_output(&terminal, styled);
    }
}

#[test]
#[ignore = "manual fixed-work performance profile"]
fn native_parser_scale_profile() {
    let samples = std::env::var("HERDR_PARSE_SAMPLES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(101)
        .max(1);
    let writes = 512;
    for styled in [false, true] {
        let stream = stream(styled);
        for panes in [1, 15] {
            let mut terminals: Vec<_> = (0..panes)
                .map(|_| Terminal::new(80, 24, 0).expect("benchmark terminal"))
                .collect();
            let mut elapsed = Vec::with_capacity(samples);
            for sample in 0..samples + 4 {
                let start = Instant::now();
                for _ in 0..writes {
                    for terminal in &mut terminals {
                        black_box(terminal).write(black_box(stream.as_bytes()));
                    }
                }
                if sample >= 4 {
                    elapsed.push(start.elapsed().as_nanos());
                }
            }
            elapsed.sort_unstable();
            for terminal in &terminals {
                check_output(terminal, styled);
            }
            eprintln!(
                "PARSE scenario={} panes={panes} writes={writes} median_ns={} p95_ns={}",
                if styled { "styled" } else { "plain" },
                elapsed[samples / 2],
                elapsed[(samples - 1) * 95 / 100]
            );
        }
    }
}
