use ghostty_vt::Terminal;

pub fn text(output: &[u8], cols: u16, rows: u16) -> String {
    let mut terminal = Terminal::new(cols, rows, 0).expect("observer terminal");
    terminal.write(output);
    terminal
        .read_text_viewport(
            (0, 0),
            (cols.saturating_sub(1), u32::from(rows.saturating_sub(1))),
            false,
        )
        .expect("observer text")
}

#[test]
fn screen_text_reconstructs_partial_redraws() {
    let output = b"REMOTE_SURVIVED\x1b[1;8HSTILL_SELECTED";
    assert!(!output
        .windows(b"REMOTE_STILL_SELECTED".len())
        .any(|part| part == b"REMOTE_STILL_SELECTED"));
    assert!(text(output, 80, 24).contains("REMOTE_STILL_SELECTED"));
}
