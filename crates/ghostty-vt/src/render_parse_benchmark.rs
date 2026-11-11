//! Parse output while the previous render snapshot stays alive, then refresh
//! and consume dirty cells through the same public API used by the renderer.
use crate::{Dirty, RenderState, RowCells, RowIterator, Terminal};
use std::{hint::black_box, time::Instant};

#[derive(Clone, Copy, Debug)]
enum Workload {
    Plain,
    Styled,
    Sparse,
    Scroll,
}
impl Workload {
    fn streams(self) -> [String; 2] {
        match self {
            Self::Plain => {
                let a = super::parse_benchmark::stream(false);
                let b = a.replace('a', "b");
                [a, b]
            }
            Self::Styled => {
                let a = super::parse_benchmark::stream(true);
                let b = a.replace("text ", "more ");
                [a, b]
            }
            Self::Sparse => [
                format!("\x1b[12;1H\x1b[38;2;17;34;51m{}\x1b[0m", "text ".repeat(16)),
                format!("\x1b[12;1H\x1b[38;2;17;34;51m{}\x1b[0m", "more ".repeat(16)),
            ],
            Self::Scroll => [
                format!(
                    "\x1b[38;2;17;34;51mlog {}\x1b[0m\r\n",
                    "e\u{301}界".repeat(20)
                ),
                format!(
                    "\x1b[38;2;17;34;51mlog {}\x1b[0m\r\n",
                    "o\u{301}語".repeat(20)
                ),
            ],
        }
    }
}
struct Pane {
    terminal: Terminal,
    snapshot: RenderState,
    rows: RowIterator,
    cells: RowCells,
    bytes: Vec<u8>,
    text: String,
}
impl Pane {
    fn new() -> Self {
        let mut pane = Self {
            terminal: Terminal::new(80, 24, 0).expect("terminal"),
            snapshot: RenderState::new().expect("snapshot"),
            rows: RowIterator::new().expect("rows"),
            cells: RowCells::new().expect("cells"),
            bytes: Vec::new(),
            text: String::with_capacity(32),
        };
        pane.terminal
            .write(super::parse_benchmark::stream(true).as_bytes());
        pane.snapshot.update(&pane.terminal).expect("snapshot");
        pane.draw();
        pane
    }
    fn verify(&self) {
        fn contents(state: &RenderState) -> Vec<(crate::CellBasicData, String)> {
            let mut iterator = RowIterator::new().expect("rows");
            let mut rows = state.populate_row_iterator(&mut iterator).expect("rows");
            let mut storage = RowCells::new().expect("cells");
            let mut result = Vec::new();
            while rows.next() {
                let mut cells = rows.populate_cells(&mut storage).expect("cells");
                while cells.next() {
                    result.push((
                        cells.basic_data().expect("cell"),
                        cells.grapheme_text().expect("text"),
                    ));
                }
            }
            result
        }
        let mut fresh = RenderState::new().expect("fresh snapshot");
        fresh.update(&self.terminal).expect("fresh snapshot");
        assert_eq!(contents(&self.snapshot), contents(&fresh));
    }
    fn draw(&mut self) -> usize {
        let mut rows = self
            .snapshot
            .populate_row_iterator(&mut self.rows)
            .expect("rows");
        let mut count = 0;
        while rows.next_dirty().is_some() {
            let mut cells = rows.populate_cells(&mut self.cells).expect("cells");
            while cells.next() {
                let basic = cells.basic_data().expect("cell data");
                cells
                    .grapheme_text_into(&mut self.bytes, &mut self.text)
                    .expect("grapheme");
                black_box((basic, self.text.as_str()));
                count += 1;
            }
            rows.clear_dirty().expect("row clean");
        }
        self.snapshot
            .set_dirty(Dirty::Clean)
            .expect("snapshot clean");
        count
    }
}
#[derive(Default)]
struct Phases {
    parse: u128,
    snapshot: u128,
    draw: u128,
    cells: usize,
}
fn frame(panes: &mut [Pane], streams: &[String; 2], frame: usize, writes: usize) -> Phases {
    let start = Instant::now();
    for pane in panes.iter_mut() {
        for n in 0..writes {
            pane.terminal
                .write(black_box(streams[(frame * writes + n) % 2].as_bytes()));
        }
    }
    let parse = start.elapsed().as_nanos();
    let start = Instant::now();
    for pane in panes.iter_mut() {
        pane.snapshot.update(&pane.terminal).expect("snapshot");
    }
    let snapshot = start.elapsed().as_nanos();
    let start = Instant::now();
    let cells = panes.iter_mut().map(Pane::draw).sum();
    Phases {
        parse,
        snapshot,
        draw: start.elapsed().as_nanos(),
        cells,
    }
}

#[test]
fn retained_render_benchmark_refreshes_dense_sparse_and_scrolling_output() {
    for workload in [
        Workload::Plain,
        Workload::Styled,
        Workload::Sparse,
        Workload::Scroll,
    ] {
        let mut panes: Vec<_> = (0..2).map(|_| Pane::new()).collect();
        for n in 0..4 {
            let phases = frame(&mut panes, &workload.streams(), n, 1);
            assert_eq!(
                phases.cells,
                if matches!(workload, Workload::Sparse) {
                    2 * 80
                } else {
                    2 * 80 * 24
                }
            );
        }
        for pane in &mut panes {
            pane.verify();
            assert_eq!(pane.draw(), 0, "clean frames must skip cell traversal");
            assert!(!pane
                .terminal
                .read_text_viewport((0, 0), (79, 23), false)
                .expect("text")
                .is_empty());
        }
    }
}

#[test]
#[ignore = "manual parsing with retained render snapshots profile"]
fn render_active_parser_scale_profile() {
    let samples = std::env::var("HERDR_RENDER_PARSE_SAMPLES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(51)
        .max(1);
    let frames = 64;
    for workload in [
        Workload::Plain,
        Workload::Styled,
        Workload::Sparse,
        Workload::Scroll,
    ] {
        let streams = workload.streams();
        for count in [1, 15] {
            for writes in [1, 8] {
                let mut panes: Vec<_> = (0..count).map(|_| Pane::new()).collect();
                let mut elapsed = Vec::with_capacity(samples);
                let mut phases = Phases::default();
                for sample in 0..samples + 4 {
                    let start = Instant::now();
                    for n in 0..frames {
                        let result = frame(&mut panes, &streams, n, writes);
                        if sample >= 4 {
                            phases.parse += result.parse;
                            phases.snapshot += result.snapshot;
                            phases.draw += result.draw;
                            phases.cells += result.cells;
                        }
                    }
                    if sample >= 4 {
                        elapsed.push(start.elapsed().as_nanos());
                    }
                }
                let (_, allocations) = crate::test_allocations::count(|| {
                    black_box(frame(&mut panes, &streams, 1, writes));
                });
                for pane in &panes {
                    pane.verify();
                }
                elapsed.sort_unstable();
                let n = (samples * frames) as u128;
                eprintln!("RENDER_PARSE workload={workload:?} panes={count} writes_per_frame={writes} frames={frames} median_ns={} p95_ns={} parse_ns={} snapshot_ns={} draw_ns={} allocations_per_frame={} cells_per_frame={}", elapsed[samples/2], elapsed[(samples-1)*95/100],phases.parse/n,phases.snapshot/n,phases.draw/n,allocations,phases.cells/(samples*frames));
            }
        }
    }
}

#[test]
fn warmed_ascii_render_cycles_do_not_allocate_at_one_or_fifteen_panes() {
    for workload in [Workload::Plain, Workload::Styled, Workload::Sparse] {
        let streams = workload.streams();
        for count in [1, 15] {
            let mut panes: Vec<_> = (0..count).map(|_| Pane::new()).collect();
            for n in 0..4 {
                frame(&mut panes, &streams, n, 1);
            }
            let (_, allocations) = crate::test_allocations::count(|| {
                black_box(frame(&mut panes, &streams, 4, 1));
            });
            assert_eq!(allocations, 0, "{workload:?}, {count} panes");
        }
    }
}
