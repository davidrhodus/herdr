use crate::*;

fn contents(state: &RenderState) -> Vec<(String, CellStyle)> {
    let mut iterator = RowIterator::new().unwrap();
    let mut rows = state.populate_row_iterator(&mut iterator).unwrap();
    let mut storage = RowCells::new().unwrap();
    let mut result = Vec::new();
    while rows.next() {
        let mut cells = rows.populate_cells(&mut storage).unwrap();
        while cells.next() {
            result.push((cells.grapheme_text().unwrap(), cells.style().unwrap()));
        }
    }
    result
}

#[test]
fn render_snapshots_remain_independent_during_unicode_and_style_writes() {
    let mut terminal = Terminal::new(8, 2, 100_000).unwrap();
    terminal.write("\x1b[31me\u{301}界".as_bytes());
    let mut a = RenderState::new().unwrap();
    let mut b = RenderState::new().unwrap();
    a.update(&terminal).unwrap();
    b.update(&terminal).unwrap();
    let retained = contents(&a);
    let terminal = std::thread::spawn(move || {
        terminal.write("\u{301}\x1b[H\x1b[38;2;17;34;51mnew".as_bytes());
        terminal
    })
    .join()
    .unwrap();
    assert_eq!(contents(&a), retained);
    assert_eq!(contents(&b), retained);
    b.update(&terminal).unwrap();
    assert_ne!(contents(&b), retained);
    assert_eq!(contents(&a), retained);
    a.update(&terminal).unwrap();
    assert_eq!(contents(&a), contents(&b));
}

#[test]
fn render_snapshot_handles_new_screens_with_repeated_row_versions() {
    let mut a = Terminal::new(4, 1, 0).unwrap();
    a.write(b"aaaa");
    let version = a.primary.lines[0].version;
    let mut state = RenderState::new().unwrap();
    state.update(&a).unwrap();
    let old = contents(&state);
    a.write(b"\x1bcbbbb");
    assert_eq!(
        a.primary.lines[0].version, version,
        "exercise revision reuse after RIS"
    );
    state.update(&a).unwrap();
    assert_ne!(contents(&state), old);
    assert_eq!(contents(&state)[0].0, "b");
    let mut b = Terminal::new(4, 1, 0).unwrap();
    b.write(b"cccc");
    assert_eq!(b.primary.lines[0].version, version);
    state.update(&b).unwrap();
    assert_eq!(contents(&state)[0].0, "c");
    b.write(b"\x1b[?1049hdddd");
    state.update(&b).unwrap();
    b.write(b"\x1b[?1049l\x1b[?1049heeee");
    state.update(&b).unwrap();
    assert_eq!(contents(&state)[0].0, "e");
}

#[test]
fn render_snapshot_tracks_scrolling_resizing_and_unchanged_dirty_rows() {
    let mut terminal = Terminal::new(8, 3, 100_000).unwrap();
    terminal.write(b"one\r\ntwo\r\nthree");
    let mut state = RenderState::new().unwrap();
    state.update(&terminal).unwrap();
    let retained = contents(&state);
    terminal.write(b"\r\nfour");
    assert_eq!(contents(&state), retained);
    state.update(&terminal).unwrap();
    assert_eq!(contents(&state)[0].0, "t");
    terminal.resize(4, 4, 0, 0).unwrap();
    state.update(&terminal).unwrap();
    assert_eq!(contents(&state).len(), 16);
    state.clean().unwrap();
    let mut iterator = RowIterator::new().unwrap();
    let mut rows = state.populate_row_iterator(&mut iterator).unwrap();
    assert!(rows.next());
    rows.set_dirty(true).unwrap();
    state.update(&terminal).unwrap();
    let mut rows = state.populate_row_iterator(&mut iterator).unwrap();
    assert!(rows.next());
    assert!(rows.dirty().unwrap());
}

#[test]
fn zero_history_scrolling_reuses_storage_without_changing_retained_snapshots() {
    let mut terminal = Terminal::new(8, 3, 0).unwrap();
    terminal.write(b"\x1b[44mx\r\nx\r\nx\r\nx\r\n");
    let mut state = RenderState::new().unwrap();
    state.update(&terminal).unwrap();
    let old = contents(&state);
    let dropped = terminal.primary.dropped;
    let (_, allocations) = crate::test_allocations::count(|| terminal.write(b"z\r\n"));
    assert_eq!(allocations, 0);
    assert_eq!(terminal.primary.history(), 0);
    assert_eq!(terminal.primary.dropped, dropped + 1);
    assert_eq!(contents(&state), old);
    state.update(&terminal).unwrap();
    assert_ne!(contents(&state), old);
    assert!(contents(&state)[16..]
        .iter()
        .all(|(text, style)| text == " " && style.bg_color == Some(CellColor::Palette(4))));
}
