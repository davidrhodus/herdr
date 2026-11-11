use super::*;
use std::sync::Arc;
use std::sync::Mutex;

fn write_numbered_lines(terminal: &mut Terminal, count: usize) {
    for i in 0..count {
        terminal.write(format!("{i:06}\r\n").as_bytes());
    }
}

fn write_padded_lines(terminal: &mut Terminal, count: usize, width: usize) {
    let line = format!("{}\r\n", "x".repeat(width));
    terminal.write(line.repeat(count).as_bytes());
}

fn first_rendered_row_text(terminal: &Terminal) -> String {
    let mut render_state = RenderState::new().unwrap();
    render_state.update(terminal).unwrap();
    let mut row_iterator = RowIterator::new().unwrap();
    let mut rows = render_state
        .populate_row_iterator(&mut row_iterator)
        .unwrap();
    let mut row_cells = RowCells::new().unwrap();
    let mut bytes = Vec::new();
    let mut cell_text = String::new();
    let mut row_text = String::new();

    assert!(rows.next());
    let mut cells = rows.populate_cells(&mut row_cells).unwrap();
    while cells.next() {
        cells
            .grapheme_text_into(&mut bytes, &mut cell_text)
            .unwrap();
        row_text.push_str(&cell_text);
    }
    row_text.trim_end().to_owned()
}

#[test]
fn incremental_compression_preserves_cold_scrollback() {
    let mut terminal = Terminal::new(80, 24, 20_000_000).unwrap();
    let initial_activity = terminal.compression_activity().unwrap();
    let suffix = "x".repeat(66);
    for line in 1..=10_000 {
        terminal.write(format!("{line:05} {suffix}\r\n").as_bytes());
    }
    assert_ne!(terminal.compression_activity().unwrap(), initial_activity);

    let mut complete = false;
    for _ in 0..10_000 {
        match terminal.compress_incremental().unwrap() {
            TerminalCompressionResult::Unsupported => return,
            TerminalCompressionResult::Pending => {}
            TerminalCompressionResult::Complete => {
                complete = true;
                break;
            }
        }
    }
    assert!(complete, "incremental compression did not converge");

    let oldest = terminal.read_text_screen((0, 0), (79, 0), false).unwrap();
    assert!(oldest.starts_with("00001 "));
    let last_row = terminal.total_rows().unwrap() as u32 - 1;
    let newest = terminal
        .read_text_screen((0, last_row - 1), (79, last_row), false)
        .unwrap();
    assert!(newest.contains("10000"));
}

#[test]
fn kitty_png_replacement_rejects_invalid_payload_without_placing_it() {
    use base64::Engine as _;
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 2, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&[255, 0, 0, 255, 0, 0, 255, 255])
            .unwrap();
    }
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    let upload = |payload: &[u8]| {
        format!(
            "\x1b[H\x1b_Ga=T,f=100,i=7,p=3,c=2,r=1,q=2;{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(payload)
        )
    };
    terminal.write(upload(&png).as_bytes());
    let before = terminal.kitty_image_placements().unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!((before[0].image_width, before[0].image_height), (2, 1));
    assert_eq!(before[0].data, [255, 0, 0, 255, 0, 0, 255, 255]);
    // The parser retires the old placement, but never places corrupt data.
    terminal.write(upload(&png[..png.len() / 2]).as_bytes());
    assert!(terminal.kitty_image_placements().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn kitty_file_image_survives_source_mutation_and_unlink() {
    use base64::Engine as _;
    let path = std::path::PathBuf::from(format!(
        "/var/tmp/herdr-kitty-source-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, [255, 0, 0, 255]).unwrap();
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.write(
        format!(
            "\x1b_Ga=T,f=32,t=f,i=7,p=3,s=1,v=1,c=1,r=1,q=2;{}\x1b\\",
            base64::engine::general_purpose::STANDARD.encode(path.to_str().unwrap())
        )
        .as_bytes(),
    );
    std::fs::write(&path, [0, 0, 255, 255]).unwrap();
    std::fs::remove_file(&path).unwrap();
    let placements = terminal.kitty_image_placements().unwrap();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].data, [255, 0, 0, 255]);
}

#[test]
fn kitty_image_fingerprint_covers_full_payload() {
    let mut data = vec![1u8; 4096 * 4];
    let original = graphics::fingerprint(&data, 100, 50, KittyImageFormat::Png);

    data[4096 + 123] = 2;
    let changed_outside_sampled_windows =
        graphics::fingerprint(&data, 100, 50, KittyImageFormat::Png);
    assert_ne!(original, changed_outside_sampled_windows);
}

#[test]
fn kitty_image_fingerprint_refreshes_on_retransmission() {
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.write(b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=10,r=5,q=2;/wAA/w==\x1b\\");
    let first = terminal
        .kitty_image_placements_with_data_filter(|_| true)
        .unwrap();
    assert_eq!(first.len(), 1);
    let first_generation = terminal
        .kitty_fingerprints
        .lock()
        .unwrap()
        .get(&7)
        .unwrap()
        .generation;
    assert_ne!(first_generation, 0);

    // Retransmission removes old placements; transmit-and-display replaces it.
    terminal.write(b"\x1b[H\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=10,r=5,q=2;AAAAAA==\x1b\\");
    let second = terminal
        .kitty_image_placements_with_data_filter(|_| true)
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_ne!(first[0].data_fingerprint, second[0].data_fingerprint);
    let second_generation = terminal
        .kitty_fingerprints
        .lock()
        .unwrap()
        .get(&7)
        .unwrap()
        .generation;
    assert_ne!(first_generation, second_generation);

    // No retransmission, so the fingerprint and generation stay stable.
    let third = terminal
        .kitty_image_placements_with_data_filter(|_| true)
        .unwrap();
    assert_eq!(second[0].data_fingerprint, third[0].data_fingerprint);
    assert_eq!(
        terminal
            .kitty_fingerprints
            .lock()
            .unwrap()
            .get(&7)
            .unwrap()
            .generation,
        second_generation
    );
}

#[test]
fn kitty_image_fingerprint_survives_hidden_placements_until_the_image_is_deleted() {
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    let cached = |terminal: &Terminal| terminal.kitty_fingerprints.lock().unwrap().contains_key(&7);
    terminal.write(b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=10,r=5,q=2;/wAA/w==\x1b\\");
    assert_eq!(
        terminal
            .kitty_image_placements_with_data_filter(|_| false)
            .unwrap()
            .len(),
        1
    );
    assert!(cached(&terminal));

    terminal.write(b"\x1b_Ga=d,d=a,q=2\x1b\\");
    assert!(terminal
        .kitty_image_placements_with_data_filter(|_| false)
        .unwrap()
        .is_empty());
    assert!(cached(&terminal), "a hidden image must not be re-hashed");

    terminal.write(b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
    let _ = terminal.kitty_image_placements_with_data_filter(|_| false);
    assert!(!cached(&terminal));
}

#[test]
fn kitty_storage_generation_skips_only_proven_empty_storage() {
    let mut terminal = Terminal::new(10, 5, 1_000_000).unwrap();
    assert!(!terminal.kitty_graphics_may_have_placements().unwrap());
    terminal.enable_kitty_graphics().unwrap();
    terminal.resize(10, 5, 8, 16).unwrap();

    let initial_generation = terminal.kitty_graphics_generation().unwrap();
    assert!(!terminal.kitty_graphics_may_have_placements().unwrap());
    assert!(terminal.kitty_image_placements().unwrap().is_empty());

    terminal.write(b"\x1b_Ga=t,t=d,f=24,i=1,s=1,v=2;////////\x1b\\");
    let transmitted = terminal.kitty_graphics_generation().unwrap();
    assert_ne!(transmitted, initial_generation);
    assert!(terminal.kitty_graphics_may_have_placements().unwrap());
    assert!(terminal.kitty_image_placements().unwrap().is_empty());
    assert_eq!(terminal.kitty_empty_generation.get(), Some(transmitted));
    assert!(!terminal.kitty_graphics_may_have_placements().unwrap());

    terminal.write(b"plain text");
    assert_eq!(terminal.kitty_graphics_generation().unwrap(), transmitted);
    assert!(terminal.kitty_image_placements().unwrap().is_empty());

    terminal.write(b"\x1b_Ga=p,i=1,p=1,c=1,r=1;\x1b\\");
    let placed = terminal.kitty_graphics_generation().unwrap();
    assert_ne!(placed, transmitted);
    assert!(terminal.kitty_graphics_may_have_placements().unwrap());
    assert_eq!(terminal.kitty_image_placements().unwrap().len(), 1);

    terminal.resize(10, 5, 12, 24).unwrap();
    assert_eq!(terminal.kitty_graphics_generation().unwrap(), placed);
    assert_eq!(terminal.kitty_image_placements().unwrap().len(), 1);

    write_numbered_lines(&mut terminal, 20);
    assert_eq!(terminal.kitty_graphics_generation().unwrap(), placed);
    assert!(terminal.kitty_image_placements().unwrap().is_empty());
    assert_ne!(terminal.kitty_empty_generation.get(), Some(placed));
    assert!(terminal.kitty_graphics_may_have_placements().unwrap());
    terminal.scroll_viewport_row(0);
    assert_eq!(terminal.kitty_image_placements().unwrap().len(), 1);

    // Delete-visible must preserve images in scrollback, even while viewed.
    terminal.write(b"\x1b_Ga=d,d=A\x1b\\");
    assert_eq!(terminal.kitty_graphics_generation().unwrap(), placed);
    assert_eq!(terminal.kitty_image_placements().unwrap().len(), 1);
    terminal.write(b"\x1b_Ga=d,d=I,i=1\x1b\\");
    let deleted = terminal.kitty_graphics_generation().unwrap();
    assert_ne!(deleted, placed);
    assert!(terminal.kitty_graphics_may_have_placements().unwrap());
    assert!(terminal.kitty_image_placements().unwrap().is_empty());
    assert_eq!(terminal.kitty_empty_generation.get(), Some(deleted));
    assert!(!terminal.kitty_graphics_may_have_placements().unwrap());
}

#[test]
fn kitty_graphics_direct_rgba_placement_is_queryable() {
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.resize(10, 5, 8, 16).unwrap();
    terminal.write(b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=10,r=5,q=2;/wAA/w==\x1b\\");

    let placements = terminal.kitty_image_placements().unwrap();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].image_id, 7);
    assert_eq!(placements[0].placement_id, 3);
    assert_eq!(placements[0].image_width, 1);
    assert_eq!(placements[0].image_height, 1);
    assert_eq!(placements[0].format, KittyImageFormat::Rgba);
    assert_eq!(placements[0].data, [255, 0, 0, 255]);
    assert_eq!(placements[0].render.grid_cols, 10);
    assert_eq!(placements[0].render.grid_rows, 5);
}

#[test]
fn kitty_graphics_queries_require_explicit_enablement() {
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    let responses = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let sink = responses.clone();
    terminal
        .set_write_pty_callback(move |bytes| sink.lock().unwrap().extend_from_slice(bytes))
        .unwrap();
    // The reporter's direct, compressed RGBA capability probe followed by DA1.
    let query = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=32,o=z;eAFjYGBgAAAABAAB\x1b\\\x1b[c";

    for enabled in [false, true] {
        if enabled {
            terminal.enable_kitty_graphics().unwrap();
        }
        for reset in [false, true] {
            if reset {
                terminal.write(b"\x1bc");
            }
            responses.lock().unwrap().clear();
            terminal.write(query);
            let expected: &[u8] = if enabled {
                b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c"
            } else {
                b"\x1b[?62;22c"
            };
            assert_eq!(
                responses.lock().unwrap().as_slice(),
                expected,
                "enabled={enabled}, reset={reset}"
            );
        }
    }
}

#[test]
fn disabled_kitty_graphics_discards_uploads_across_screen_changes() {
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    for sequence in [b"".as_slice(), b"\x1b[?1049h", b"\x1b[?1049l"] {
        terminal.write(sequence);
        terminal.resize(12, 6, 8, 16).unwrap();
        terminal.write(b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1;/wAA/w==\x1b\\");
        assert!(terminal.kitty_image_placements().unwrap().is_empty());
    }
}

#[test]
fn kitty_graphics_file_medium_rgba_placement_is_queryable() {
    use base64::Engine;

    let dir = std::env::temp_dir().join(format!(
        "herdr-kitty-file-medium-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pixel.rgba");
    std::fs::write(&path, [255, 0, 0, 255]).unwrap();

    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.resize(10, 5, 8, 16).unwrap();
    let encoded_path =
        base64::engine::general_purpose::STANDARD.encode(path.as_os_str().as_encoded_bytes());
    let command = format!("\x1b_Ga=T,f=32,t=f,i=9,p=4,s=1,v=1,c=10,r=5,q=2;{encoded_path}\x1b\\");
    terminal.write(command.as_bytes());
    terminal.write(b"\x1b_Ga=p,U=1,i=9,c=10,r=5\x1b\\");

    let placements = terminal.kitty_image_placements().unwrap();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].image_id, 9);
    assert_eq!(placements[0].placement_id, 4);
    assert_eq!(placements[0].image_width, 1);
    assert_eq!(placements[0].image_height, 1);
    assert_eq!(placements[0].format, KittyImageFormat::Rgba);
    assert_eq!(placements[0].data, [255, 0, 0, 255]);
    assert_eq!(placements[0].render.grid_cols, 10);
    assert_eq!(placements[0].render.grid_rows, 5);

    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn kitty_graphics_file_upload_can_be_placed_later() {
    use base64::Engine as _;

    let dir = std::env::temp_dir().join(format!(
        "herdr-kitty-file-upload-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pixel.rgba");
    std::fs::write(&path, [255, 0, 0, 255]).unwrap();

    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.resize(10, 5, 8, 16).unwrap();
    let payload =
        base64::engine::general_purpose::STANDARD.encode(path.to_str().unwrap().as_bytes());
    let upload = format!("\x1b7\x1b_Ga=t,f=32,s=1,v=1,i=10,q=0,t=f;{payload}\x1b\\\x1b8");
    terminal.write(upload.as_bytes());
    assert!(terminal.kitty_image_placements().unwrap().is_empty());

    terminal.write(b"\x1b_Ga=p,i=10,p=5,c=10,r=5,C=1,q=2\x1b\\");
    let placements = terminal.kitty_image_placements().unwrap();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].image_id, 10);
    assert_eq!(placements[0].placement_id, 5);
    assert_eq!(placements[0].image_width, 1);
    assert_eq!(placements[0].image_height, 1);
    assert_eq!(placements[0].format, KittyImageFormat::Rgba);
    assert_eq!(placements[0].data, [255, 0, 0, 255]);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn kitty_graphics_unicode_placeholder_placement_is_queryable() {
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.resize(10, 5, 8, 16).unwrap();
    terminal.write(b"\x1b_Gq=2,a=t,t=d,f=32,s=1,v=1,i=1193046,m=0;/wAA/w==\x1b\\");
    terminal.write(b"\x1b_Gq=2,a=p,U=1,i=1193046,c=2,r=1\x1b\\");
    terminal.write(
        "\x1b[2;3H\x1b[38;2;18;52;86m\u{10eeee}\u{0305}\u{0305}\u{10eeee}\u{0305}\u{030d}\x1b[0m"
            .as_bytes(),
    );

    let placements = terminal.kitty_image_placements().unwrap();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].image_id, 1193046);
    assert_ne!(placements[0].placement_id, 0);
    assert_eq!(placements[0].image_width, 1);
    assert_eq!(placements[0].image_height, 1);
    assert_eq!(placements[0].format, KittyImageFormat::Rgba);
    assert_eq!(placements[0].data, [255, 0, 0, 255]);
    assert_eq!(placements[0].render.viewport_col, 2);
    assert_eq!(placements[0].render.viewport_row, 1);
    assert_eq!(placements[0].render.grid_cols, 2);
    assert_eq!(placements[0].render.grid_rows, 1);
}

#[test]
fn unicode_width_helpers_match_terminal_layout_rules() {
    assert_eq!(unicode_codepoint_width('A' as u32), 1);
    assert_eq!(unicode_codepoint_width('\u{301}' as u32), 0);
    assert_eq!(unicode_codepoint_width('界' as u32), 2);
    assert_eq!(unicode_codepoint_width(0x11_0000), 1);

    let cases: &[(&[u32], usize, u8)] = &[
        (&[], 0, 0),
        (&['e' as u32, '\u{301}' as u32], 2, 1),
        (&['⚠' as u32, '\u{fe0f}' as u32], 2, 2),
        (&['⚠' as u32, '\u{fe0e}' as u32], 2, 1),
        (&['🇧' as u32, '🇷' as u32], 2, 2),
        (&['👍' as u32, '🏽' as u32], 2, 2),
        (
            &[
                '👨' as u32,
                '\u{200d}' as u32,
                '👩' as u32,
                '\u{200d}' as u32,
                '👧' as u32,
            ],
            5,
            2,
        ),
        (&[0x11_0000, 'A' as u32], 1, 1),
    ];
    for &(codepoints, consumed, width) in cases {
        assert_eq!(unicode_grapheme_width(codepoints), (consumed, width));
    }
}

#[test]
fn focus_encoding_matches_expected_sequences() {
    assert_eq!(encode_focus(FocusEvent::Gained).unwrap(), b"\x1b[I");
    assert_eq!(encode_focus(FocusEvent::Lost).unwrap(), b"\x1b[O");
}

#[test]
fn terminal_callbacks_report_pty_responses_and_pwd_changes() {
    let mut terminal = Terminal::new(8, 3, 100).unwrap();
    let responses = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let sink = responses.clone();
    terminal
        .set_write_pty_callback(move |bytes| sink.lock().unwrap().extend_from_slice(bytes))
        .unwrap();

    terminal.write(b"\x1b[6n\x1b]7;file:///tmp/herdr\x07");

    let output = responses.lock().unwrap().clone();
    assert!(!output.is_empty());
    assert!(String::from_utf8_lossy(&output).contains("R"));
    assert_eq!(terminal.take_pwd_changes(), [b"file:///tmp/herdr".to_vec()]);
}

#[test]
fn key_and_mouse_encoders_follow_terminal_state() {
    let mut terminal = Terminal::new(80, 24, 0).unwrap();
    terminal.mode_set(1, true).unwrap();
    terminal.write(b"\x1b[>1u\x1b[?1000h\x1b[?1006h");

    assert!(terminal.mode_get(1).unwrap());
    assert_eq!(terminal.kitty_keyboard_flags().unwrap(), 1);
    assert!(terminal.mouse_tracking_enabled().unwrap());

    let mut key_encoder = KeyEncoder::new().unwrap();
    key_encoder.set_from_terminal(&terminal);
    let mut key_event = KeyEvent::new().unwrap();
    key_event.set_action(ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS);
    key_event.set_key(KEY_A);
    key_event.set_mods(MOD_CTRL | MOD_SHIFT);
    key_event.set_utf8("A");
    key_event.set_unshifted_codepoint('a' as u32);
    let encoded_key = key_encoder.encode(&key_event).unwrap();
    assert_eq!(encoded_key, b"\x1b[97;6u");

    let mut mouse_encoder = MouseEncoder::new().unwrap();
    mouse_encoder.set_from_terminal(&terminal);
    mouse_encoder.set_size(80, 24, 1, 1);
    let mut mouse_event = MouseEvent::new().unwrap();
    mouse_event.set_action(ffi::GhosttyMouseAction_GHOSTTY_MOUSE_ACTION_PRESS);
    mouse_event.set_button(ffi::GhosttyMouseButton_GHOSTTY_MOUSE_BUTTON_LEFT);
    mouse_event.set_position(0.0, 0.0);
    let encoded_mouse = mouse_encoder.encode(&mouse_event).unwrap();
    assert_eq!(encoded_mouse, b"\x1b[<0;1;1M");
}

#[test]
fn modify_other_keys_query_tracks_mode_two() {
    let mut terminal = Terminal::new(80, 24, 0).unwrap();

    assert!(!terminal.modify_other_keys_enabled().unwrap());
    terminal.write(b"\x1b[>4;2m");
    assert!(terminal.modify_other_keys_enabled().unwrap());
    terminal.write(b"\x1b[>4;0m");
    assert!(!terminal.modify_other_keys_enabled().unwrap());
}

#[test]
fn link_target_includes_offscreen_wrapped_rows() {
    let url = "https://example.com/abcdefghijklmnopqrstuv";
    let mut terminal = Terminal::new(20, 2, 1024 * 1024).unwrap();
    terminal.write(url.as_bytes());
    let expected = LinkTarget::Text {
        text: url.to_owned(),
        clicked_byte: 20,
    };
    assert_eq!(terminal.viewport_link_target(0, 0).unwrap(), Some(expected));
    terminal.scroll_viewport_row(0);
    assert_eq!(
        terminal.viewport_link_target(0, 0).unwrap(),
        Some(LinkTarget::Text {
            text: url.to_owned(),
            clicked_byte: 0
        })
    );
}

#[test]
fn link_target_prefers_explicit_uri_and_does_not_join_hard_lines() {
    let mut terminal = Terminal::new(20, 4, 1024).unwrap();
    terminal.write(b"\x1b]8;;https://example.com/real\x1b\\label\x1b]8;;\x1b\\\r\nhttps://example.com/\r\ntail");
    assert_eq!(
        terminal.viewport_link_target(2, 0).unwrap(),
        Some(LinkTarget::Uri("https://example.com/real".into()))
    );
    assert_eq!(
        terminal.viewport_link_target(1, 2).unwrap(),
        Some(LinkTarget::Text {
            text: "tail".into(),
            clicked_byte: 1
        })
    );
}

#[test]
fn link_target_stops_at_hard_line_when_clicking_last_column() {
    let mut terminal = Terminal::new(20, 3, 1024).unwrap();
    terminal.write(b"https://example.com/x\r\nnot-part-of-url");
    assert_eq!(
        terminal.viewport_link_target(19, 0).unwrap(),
        Some(LinkTarget::Text {
            text: "https://example.com/x".into(),
            clicked_byte: 19
        })
    );
}

#[test]
fn link_target_wide_explicit_label_works_on_both_cell_halves() {
    let mut terminal = Terminal::new(20, 2, 1024).unwrap();
    terminal.write("\x1b]8;;https://example.com/\x1b\\路径\x1b]8;;\x1b\\".as_bytes());
    for col in 0..4 {
        assert_eq!(
            terminal.viewport_link_target(col, 0).unwrap(),
            Some(LinkTarget::Uri("https://example.com/".into()))
        );
    }
}

#[test]
fn link_target_selection_budget_is_shared_across_both_directions() {
    let mut terminal = Terminal::new(20, 2, 1024).unwrap();
    terminal.write(b" abc ");
    for budget in 0..=5 {
        let selected = terminal.token(2, 0, budget);
        if budget < 5 {
            assert!(selected.is_none(), "budget={budget}");
        } else {
            assert_eq!(selected.unwrap().0, "abc");
        }
    }
}

#[test]
fn link_target_rejects_oversized_token_without_returning_a_prefix() {
    let mut terminal = Terminal::new(100, 4, 16 * 1024 * 1024).unwrap();
    let text = format!("https://example.com/{}", "a".repeat(1_000_000));
    terminal.write(text.as_bytes());
    let started = std::time::Instant::now();
    let target = terminal.viewport_link_target(0, 0).unwrap();
    eprintln!("million-byte token lookup: {:?}", started.elapsed());
    assert!(target.is_none());
}

#[test]
fn terminal_read_text_viewport_unwraps_soft_wrapped_selection() {
    let mut terminal = Terminal::new(5, 3, 0).unwrap();
    terminal.write("1ABCD2EFGH3IJKL".as_bytes());

    let text = terminal.read_text_viewport((0, 1), (2, 2), false).unwrap();
    assert_eq!(text, "2EFGH3IJ");
}

#[test]
fn terminal_extracts_viewport_hyperlink_uri() {
    let mut terminal = Terminal::new(20, 3, 0).unwrap();
    terminal.write(b"\x1b]8;;https://example.com\x1b\\Link\x1b]8;;\x1b\\");

    assert_eq!(
        terminal.viewport_hyperlink_uri(0, 0).unwrap().as_deref(),
        Some("https://example.com")
    );
    assert_eq!(terminal.viewport_hyperlink_uri(4, 0).unwrap(), None);
}

#[test]
fn terminal_read_text_viewport_handles_wide_chars() {
    let mut terminal = Terminal::new(5, 3, 0).unwrap();
    terminal.write("1A⚡".as_bytes());

    let full = terminal.read_text_viewport((0, 0), (3, 0), false).unwrap();
    assert_eq!(full, "1A⚡");

    let through_wide_head = terminal.read_text_viewport((0, 0), (2, 0), false).unwrap();
    assert_eq!(through_wide_head, "1A⚡");

    let wide_only = terminal.read_text_viewport((3, 0), (3, 0), false).unwrap();
    assert_eq!(wide_only, "⚡");
}

#[test]
fn zero_max_scrollback_disables_history() {
    let mut terminal = Terminal::new(80, 3, 0).unwrap();
    write_numbered_lines(&mut terminal, 3000);
    assert_eq!(terminal.scrollback_rows().unwrap(), 0);
}

#[test]
fn max_scrollback_limit_bytes_retains_more_history_for_larger_limits() {
    let mut small = Terminal::new(80, 3, 1_000_000).unwrap();
    let mut large = Terminal::new(80, 3, 10_000_000).unwrap();

    write_padded_lines(&mut small, 1_250, 70);
    write_padded_lines(&mut large, 1_250, 70);

    let small_scrollback = small.scrollback_rows().unwrap();
    let large_scrollback = large.scrollback_rows().unwrap();

    assert!(
            large_scrollback > small_scrollback,
            "expected larger byte limit to retain more history, got small={small_scrollback}, large={large_scrollback}"
        );
}

#[test]
fn large_negative_scroll_delta_reaches_top_of_scrollback() {
    let mut terminal = Terminal::new(80, 3, 1_000_000).unwrap();
    write_numbered_lines(&mut terminal, 1000);

    let before = terminal.scrollbar().unwrap();
    assert!(before.total > before.len);

    terminal.scroll_viewport_bottom();
    terminal.scroll_viewport_delta(-10_000);

    let after = terminal.scrollbar().unwrap();
    assert_eq!(after.offset, 0);
    assert_eq!(after.len, before.len);
}

#[test]
fn absolute_scroll_row_round_trips_and_clamps() {
    let mut terminal = Terminal::new(80, 3, 1_000_000).unwrap();
    write_numbered_lines(&mut terminal, 1000);

    let before = terminal.scrollbar().unwrap();
    let max_row = before.total.saturating_sub(before.len);
    assert!(max_row > 0);

    for row in [0, max_row / 2, max_row, usize::MAX] {
        terminal.scroll_viewport_row(row);
        let after = terminal.scrollbar().unwrap();
        assert_eq!(after.offset, row.min(max_row));
        assert_eq!(after.len, before.len);
    }
}

#[test]
fn deep_scrollback_resize_preserves_unicode_and_hyperlinks() {
    use std::fmt::Write as _;

    let mut terminal = Terminal::new(20, 5, 100_000_000).unwrap();
    let mut input = String::from("\x1b]8;;https://example.com\x1b\\FIRST 🇧🇷\x1b]8;;\x1b\\\r\n");
    for line in 0..70_000 {
        writeln!(input, "{line:05} 👨‍👩‍👧").unwrap();
    }
    terminal.write(input.as_bytes());

    assert!(terminal.scrollback_rows().unwrap() > u16::MAX as usize);
    terminal.scroll_viewport_delta(-100_000);
    assert_eq!(terminal.scrollbar().unwrap().offset, 0);
    assert!(terminal
        .read_text_viewport((0, 0), (19, 0), false)
        .unwrap()
        .starts_with("FIRST 🇧🇷"));
    assert_eq!(
        terminal.viewport_hyperlink_uri(0, 0).unwrap().as_deref(),
        Some("https://example.com")
    );

    terminal.resize(10, 5, 8, 16).unwrap();
    terminal.scroll_viewport_delta(-100_000);
    let metrics = terminal.scrollbar().unwrap();
    assert_eq!(metrics.offset, 0);
    assert_eq!(metrics.len, 5);
    assert!(terminal
        .read_text_viewport((0, 0), (9, 0), false)
        .unwrap()
        .starts_with("FIRST 🇧🇷"));
    assert_eq!(
        terminal.viewport_hyperlink_uri(0, 0).unwrap().as_deref(),
        Some("https://example.com")
    );
}

#[test]
fn raw_resize_preserves_content_without_replaying_terminal_effects() {
    let mut terminal = Terminal::new(20, 6, 100_000).unwrap();
    terminal.write(b"header\r\n\x1b[6;1Htail\x1b[6;18H");
    for (cols, rows) in [(10, 3), (30, 8), (8, 4), (20, 6)] {
        terminal.resize(cols, rows, 8, 16).unwrap();
        terminal.write(b"X");
        let text = terminal
            .read_text_screen(
                (0, 0),
                (cols - 1, terminal.total_rows().unwrap() as u32 - 1),
                false,
            )
            .unwrap();
        assert!(
            text.contains("tail"),
            "lost tail after {cols}x{rows}: {text:?}"
        );
        assert_eq!(text.matches("tail").count(), 1);
        assert!(terminal.cursor_y().unwrap() < rows);
    }
    terminal.write(b"\x1b[2J\x1b[H");
    terminal.resize(12, 3, 8, 16).unwrap();
    assert!(terminal
        .read_text_viewport((0, 0), (11, 2), false)
        .unwrap()
        .trim()
        .is_empty());
}

#[test]
fn clipboard_queries_never_disclose_contents_and_split_writes_complete_once() {
    for suffix in [b"\x07".as_slice(), b"\x1b\\".as_slice()] {
        let bytes = [b"\x1b]52;c;YQBi".as_slice(), suffix].concat();
        for split in 0..=bytes.len() {
            let mut terminal = Terminal::new(10, 3, 0).unwrap();
            let replies = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&replies);
            terminal
                .set_write_pty_callback(move |bytes| {
                    captured.lock().unwrap().extend_from_slice(bytes)
                })
                .unwrap();
            terminal.write(&bytes[..split]);
            terminal.write(&bytes[split..]);
            assert_eq!(terminal.take_clipboard_writes(), vec![b"a\0b".to_vec()]);
            terminal.write(b"\x1b]52;c;?\x07\x1b]52;p;YQBi\x07");
            assert!(terminal.take_clipboard_writes().is_empty());
            assert!(replies.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn active_screen_and_cursor_visibility_contract() {
    let mut terminal = Terminal::new(12, 3, 0).unwrap();
    let mut render_state = RenderState::new().unwrap();

    terminal.write(b"primary");
    assert_eq!(terminal.active_screen().unwrap(), ActiveScreen::Primary);
    assert_eq!(
        terminal.read_text_viewport((0, 0), (6, 0), false).unwrap(),
        "primary"
    );

    render_state.update(&terminal).unwrap();
    assert!(render_state.cursor().unwrap().visible);
    terminal.write(b"\x1b[?25l");
    render_state.update(&terminal).unwrap();
    assert!(!render_state.cursor().unwrap().visible);

    terminal.write(b"\x1b[?1049h\x1b[HALT");
    assert_eq!(terminal.active_screen().unwrap(), ActiveScreen::Alternate);
    assert_eq!(
        terminal.read_text_viewport((0, 0), (2, 0), false).unwrap(),
        "ALT"
    );

    terminal.write(b"\x1b[?1049l");
    assert_eq!(terminal.active_screen().unwrap(), ActiveScreen::Primary);
    assert_eq!(
        terminal.read_text_viewport((0, 0), (6, 0), false).unwrap(),
        "primary"
    );
}

#[test]
fn terminal_and_render_state_smoke_test() {
    let mut terminal = Terminal::new(8, 3, 100).unwrap();
    assert_eq!(terminal.cols().unwrap(), 8);
    assert_eq!(terminal.rows().unwrap(), 3);

    terminal.write(b"hello\r\nworld");

    let mut render_state = RenderState::new().unwrap();
    render_state.update(&terminal).unwrap();
    assert_eq!(render_state.cols().unwrap(), 8);
    assert_eq!(render_state.rows().unwrap(), 3);
    assert_ne!(render_state.dirty().unwrap(), Dirty::Clean);

    let mut row_iterator = RowIterator::new().unwrap();
    let mut row_iter = render_state
        .populate_row_iterator(&mut row_iterator)
        .unwrap();
    let mut row_cells = RowCells::new().unwrap();

    let mut found_hello = false;
    let mut found_world = false;
    let mut row_index = 0usize;
    while row_iter.next() {
        let _ = row_iter.dirty().unwrap();
        let mut cells = row_iter.populate_cells(&mut row_cells).unwrap();
        let mut line = String::new();
        while cells.next() {
            let text = cells.grapheme_text().unwrap();
            if text.is_empty() {
                line.push(' ');
            } else {
                line.push_str(&text);
            }
        }
        let trimmed = line.trim_end().to_string();
        if row_index == 0 {
            found_hello = trimmed.starts_with("hello");
        }
        if row_index == 1 {
            found_world = trimmed.starts_with("world");
        }
        row_index += 1;
    }

    assert!(found_hello);
    assert!(found_world);

    render_state.set_dirty(Dirty::Clean).unwrap();
    assert_eq!(render_state.dirty().unwrap(), Dirty::Clean);
}

#[test]
fn render_cells_preserve_issue_453_unicode_payload_exactly() {
    const PAYLOAD: &str = "README 👨‍👩‍👧‍👦 🧑‍💻 ✅ ⚡ 漢字 café é 🏳️‍🌈 🚀";
    let mut terminal = Terminal::new(80, 3, 100).unwrap();
    assert!(terminal.mode_get(MODE_GRAPHEME_CLUSTER).unwrap());
    terminal.write(format!("{PAYLOAD}\r\n").as_bytes());

    assert_eq!(first_rendered_row_text(&terminal), PAYLOAD);
}

#[test]
fn grapheme_cluster_mode_is_default_and_survives_full_reset() {
    let mut terminal = Terminal::new(80, 3, 100).unwrap();
    assert!(terminal.mode_get(MODE_GRAPHEME_CLUSTER).unwrap());
    terminal.mode_set(MODE_GRAPHEME_CLUSTER, false).unwrap();
    assert!(!terminal.mode_get(MODE_GRAPHEME_CLUSTER).unwrap());

    terminal.write(b"\x1bc");

    assert!(terminal.mode_get(MODE_GRAPHEME_CLUSTER).unwrap());
}

#[test]
fn screen_text_rows_preserve_wrap_and_grapheme_cells() {
    let mut terminal = Terminal::new(5, 3, 100).unwrap();
    terminal.write("abcdef\r\n界e\u{301}".as_bytes());

    let rows = terminal.screen_text_rows().unwrap();

    assert_eq!(rows.len(), 3);
    assert!(rows[0].soft_wrapped);
    assert!(!rows[0].wrap_continuation);
    assert!(!rows[1].soft_wrapped);
    assert!(rows[1].wrap_continuation);
    assert!(!rows[2].wrap_continuation);
    assert_eq!(rows[2].cells[0].wide, CellWide::Wide);
    assert_eq!(rows[2].cells[0].graphemes, vec!['界' as u32]);
    assert_eq!(rows[2].cells[1].wide, CellWide::SpacerTail);
    assert_eq!(rows[2].cells[2].graphemes, vec!['e' as u32, 0x301]);
}

#[test]
fn owned_cell_graphemes_keep_tight_capacity() {
    let mut terminal = Terminal::new(8, 1, 100).unwrap();
    terminal.write("xe\u{301}".as_bytes());

    let rows = terminal.screen_text_rows().unwrap();
    assert_eq!(rows[0].cells[0].graphemes, vec!['x' as u32]);
    assert_eq!(rows[0].cells[1].graphemes, vec!['e' as u32, 0x301]);
    for cell in &rows[0].cells {
        assert_eq!(cell.graphemes.capacity(), cell.graphemes.len());
    }
}

#[test]
fn borrowed_screen_row_cells_match_owned_cells() {
    let mut terminal = Terminal::new(12, 4, 100).unwrap();
    let content = format!(
        "abcdefghi界Z\r\ne{} X\r\n{}\r\n",
        "\u{301}".repeat(40),
        char::from_u32(KITTY_UNICODE_PLACEHOLDER).unwrap()
    );
    for screen in ["", "\x1b[?1049h"] {
        terminal.write(screen.as_bytes());
        terminal.write(content.as_bytes());
        let expected = terminal.screen_text_rows().unwrap();
        assert!(expected
            .iter()
            .flat_map(|row| &row.cells)
            .any(|cell| cell.graphemes.len() > 4));
        for (y, row) in expected.iter().enumerate() {
            let mut actual = Vec::new();
            terminal
                .for_each_screen_row_cell(y as u32, |wide, graphemes| {
                    actual.push((wide, graphemes.to_vec()));
                })
                .unwrap();
            let expected: Vec<_> = row
                .cells
                .iter()
                .map(|cell| (cell.wide, cell.graphemes.clone()))
                .collect();
            assert_eq!(actual, expected);
        }
        terminal
            .for_each_screen_row_cell(u32::MAX, |_, _| panic!("out-of-range row"))
            .unwrap();
    }
}

#[test]
fn borrowed_screen_row_allocations_do_not_scale_with_cell_count() {
    for cols in [80, 330] {
        let mut terminal = Terminal::new(cols, 1, 100).unwrap();
        terminal.write("x".repeat(usize::from(cols)).as_bytes());
        let mut cells = 0;
        let (result, allocations) = crate::test_allocations::count(|| {
            terminal.for_each_screen_row_cell(0, |_, graphemes| {
                std::hint::black_box(graphemes);
                cells += 1;
            })
        });
        result.unwrap();
        assert_eq!(cells, cols);
        assert!(
                allocations <= 1,
                "one reusable grapheme buffer per ASCII row, got {allocations} allocations for {cols} cells"
            );
    }
}

#[test]
fn render_state_row_dirty_can_be_cleared_independently() {
    let mut terminal = Terminal::new(8, 3, 100).unwrap();
    let mut render_state = RenderState::new().unwrap();

    render_state.update(&terminal).unwrap();
    {
        let mut row_iterator = RowIterator::new().unwrap();
        let mut rows = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        while rows.next() {
            rows.clear_dirty().unwrap();
            assert!(!rows.dirty().unwrap());
        }
    }
    {
        let mut iterator = RowIterator::new().unwrap();
        let mut rows = render_state.populate_row_iterator(&mut iterator).unwrap();
        assert_eq!(rows.next_dirty(), Some(0));
        assert_eq!(rows.next_dirty(), Some(1));
        assert_eq!(rows.next_dirty(), Some(2));
        assert_eq!(rows.next_dirty(), None);
    }
    render_state.set_dirty(Dirty::Clean).unwrap();
    assert_eq!(render_state.dirty().unwrap(), Dirty::Clean);
    {
        let mut iterator = RowIterator::new().unwrap();
        let mut rows = render_state.populate_row_iterator(&mut iterator).unwrap();
        assert_eq!(rows.next_dirty(), None);
    }

    terminal.write(b"A");
    render_state.update(&terminal).unwrap();
    assert_eq!(render_state.dirty().unwrap(), Dirty::Partial);

    let mut dirty_rows = 0usize;
    {
        let mut row_iterator = RowIterator::new().unwrap();
        let mut rows = render_state
            .populate_row_iterator(&mut row_iterator)
            .unwrap();
        while let Some(y) = rows.next_dirty() {
            assert_eq!(y, 0);
            assert!(rows.dirty().unwrap());
            dirty_rows += 1;
            rows.clear_dirty().unwrap();
            assert!(!rows.dirty().unwrap());
        }
    }
    assert_eq!(dirty_rows, 1);
    assert_eq!(render_state.dirty().unwrap(), Dirty::Partial);

    render_state.set_dirty(Dirty::Clean).unwrap();
    assert_eq!(render_state.dirty().unwrap(), Dirty::Clean);
}

#[test]
fn row_selection_returns_none_without_selection() {
    let terminal = Terminal::new(8, 3, 100).unwrap();
    let mut render_state = RenderState::new().unwrap();
    render_state.update(&terminal).unwrap();

    let mut row_iterator = RowIterator::new().unwrap();
    let mut rows = render_state
        .populate_row_iterator(&mut row_iterator)
        .unwrap();
    assert!(rows.next());
    assert_eq!(rows.selection().unwrap(), None);
}

#[test]
fn libghostty_completes_osc52_writes_for_bel_and_st_without_queries() {
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    terminal.write(b"\x1b]52;c;aGVs");
    assert!(terminal.take_clipboard_writes().is_empty());
    terminal.write(b"bG8=\x07");
    assert_eq!(terminal.take_clipboard_writes(), vec![b"hello".to_vec()]);

    terminal.write(b"\x1b]52;c;d29ybGQ=\x1b\\");
    assert_eq!(terminal.take_clipboard_writes(), vec![b"world".to_vec()]);

    terminal.write(b"\x1b]52;c;?\x07");
    assert!(terminal.take_clipboard_writes().is_empty());

    terminal.write(b"\x1b]52;c;\x07");
    assert!(terminal.take_clipboard_writes().is_empty());
}

#[test]
fn row_cell_basic_data_uses_batched_vendor_reads() {
    let mut terminal = Terminal::new(8, 3, 100).unwrap();
    terminal.write(b"\x1b[31mA\x1b[0m");

    let mut render_state = RenderState::new().unwrap();
    render_state.update(&terminal).unwrap();

    let mut row_iterator = RowIterator::new().unwrap();
    let mut rows = render_state
        .populate_row_iterator(&mut row_iterator)
        .unwrap();
    assert!(rows.next());

    let mut row_cells = RowCells::new().unwrap();
    let mut cells = rows.populate_cells(&mut row_cells).unwrap();
    assert!(cells.next());

    let basic = cells.basic_data().unwrap();
    assert_eq!(basic.wide, CellWide::Narrow);
    assert!(basic.has_styling);
    assert_eq!(basic.style.fg_color, Some(CellColor::Palette(1)));
    assert!(!basic.has_hyperlink);
}

#[test]
fn malformed_output_preserves_grid_bounds_and_parser_recovery() {
    let mut terminal = Terminal::new(1, 2, 0).unwrap();
    terminal.write("界👩‍💻\tX".as_bytes());
    terminal.resize(80, 3, 8, 16).unwrap();
    terminal.resize(4, 3, 8, 16).unwrap();
    terminal.write(b"\tX\x1b[IY\x1b[H");
    terminal.write("\x1b]10;#€\x07\x1b]4;0;rgb:€/0/0\x07".as_bytes());
    terminal.write(b"\x1b_Garbage\x18OK");
    assert!(terminal
        .read_text_viewport((0, 0), (3, 2), false)
        .unwrap()
        .starts_with("OK"));
    let rows = terminal.screen_text_rows().unwrap();
    assert!(rows.iter().all(|row| row.cells.len() == 4));
}

#[test]
fn alternate_mode_47_preserves_contents_between_switches() {
    let mut terminal = Terminal::new(12, 3, 100_000).unwrap();
    terminal.write(b"primary\x1b[?47hother\x1b[?47l\x1b[?47h");
    assert_eq!(
        terminal.read_text_viewport((0, 0), (4, 0), false).unwrap(),
        "other"
    );
}

#[test]
fn overlapping_and_inserted_wide_cells_keep_paired_spacers() {
    let mut terminal = Terminal::new(8, 3, 0).unwrap();
    for bytes in ["a界Z\r界", "\x1b[H界界\r\x1b[@", "\x1b[H界界\r\x1b[P"] {
        terminal.write(bytes.as_bytes());
        let rows = terminal.screen_text_rows().unwrap();
        for row in rows {
            for (x, cell) in row.cells.iter().enumerate() {
                match cell.wide {
                    CellWide::Wide => assert!(row
                        .cells
                        .get(x + 1)
                        .is_some_and(|c| c.wide == CellWide::SpacerTail)),
                    CellWide::SpacerTail => {
                        assert!(x > 0 && row.cells[x - 1].wide == CellWide::Wide)
                    }
                    _ => {}
                }
            }
        }
    }
    terminal.resize(1, 3, 8, 16).unwrap();
    assert!(terminal
        .screen_text_rows()
        .unwrap()
        .iter()
        .all(|row| !matches!(row.cells[0].wide, CellWide::Wide | CellWide::SpacerTail)));
}

#[test]
fn graphics_rejects_overflowing_dimensions_and_bounds_storage_metadata() {
    let mut terminal = Terminal::new(20, 4, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.write(b"\x1b_Ga=T,f=32,i=9,s=4294967295,v=4294967295,q=2;AAAA\x1b\\");
    assert!(terminal.kitty_image_placements().unwrap().is_empty());
    terminal.write(b"\x1b_Ga=T,f=32,i=1,s=1,v=1,C=1,q=2;AAAAAA==\x1b\\");
    for id in 1..5000 {
        terminal.write(format!("\x1b_Ga=p,i=1,p={id},C=1,q=2;\x1b\\").as_bytes());
    }
    assert!(terminal.kitty_image_placements().unwrap().len() <= 4096);
}

#[test]
fn native_scrollback_accounts_for_owned_cell_and_grapheme_bytes() {
    let budget = 2 * 1024 * 1024;
    let mut terminal = Terminal::new(80, 3, budget).unwrap();
    let line = format!("{}\r\n", "e\u{301}".repeat(70));
    for _ in 0..4000 {
        terminal.write(line.as_bytes());
    }
    let actual: usize = terminal
        .primary
        .lines
        .iter()
        .take(terminal.primary.history())
        .map(|row| row.bytes())
        .sum();
    assert_eq!(actual, terminal.primary.history_bytes);
    assert!(actual <= budget);
}

#[test]
fn cold_rows_compress_and_expand_without_losing_cells() {
    let mut terminal = Terminal::new(80, 4, 2_000_000).unwrap();
    for _ in 0..400 {
        terminal.write(b"prefix xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\n");
    }
    let before_text = terminal
        .read_text_screen(
            (0, 0),
            (79, terminal.primary.lines.len().saturating_sub(1) as u32),
            false,
        )
        .unwrap();
    let before: usize = terminal
        .primary
        .lines
        .iter()
        .map(|r| r.cells.storage_bytes())
        .sum();
    while terminal.compress_incremental().unwrap() == TerminalCompressionResult::Pending {}
    let after: usize = terminal
        .primary
        .lines
        .iter()
        .map(|r| r.cells.storage_bytes())
        .sum();
    assert!(after < before / 2);
    assert_eq!(
        terminal
            .read_text_screen(
                (0, 0),
                (79, terminal.primary.lines.len().saturating_sub(1) as u32),
                false
            )
            .unwrap(),
        before_text
    );
    let accounted: usize = terminal
        .primary
        .lines
        .iter()
        .take(terminal.primary.history())
        .map(|row| row.bytes())
        .sum();
    assert_eq!(terminal.primary.history_bytes, accounted);
    terminal.resize(40, 4, 0, 0).unwrap();
    terminal.resize(80, 4, 0, 0).unwrap();
    assert!(terminal
        .read_text_screen(
            (0, 0),
            (79, terminal.primary.lines.len().saturating_sub(1) as u32),
            false
        )
        .unwrap()
        .contains("prefix"));
}

#[test]
fn graphics_follow_partial_scroll_and_remain_separate_between_screens() {
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.write(b"\x1b[3;1H\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2,C=1;/wAA/w==\x1b\\");
    let original = terminal.graphics.anchors(&terminal.primary)[0].1 .0;
    terminal.write(b"\x1b[2;4r\x1b[1S");
    assert_eq!(
        terminal.graphics.anchors(&terminal.primary)[0].1 .0,
        original - 1
    );
    terminal.write(b"\x1b[?1049h\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2;AP8A/w==\x1b\\");
    assert_eq!(
        terminal.kitty_image_placements().unwrap()[0].data,
        [0, 255, 0, 255]
    );
    terminal.write(b"\x1b[?1049l");
    assert_eq!(
        terminal.kitty_image_placements().unwrap()[0].data,
        [255, 0, 0, 255]
    );
}

#[test]
fn growing_edge_grapheme_keeps_wide_cells_paired() {
    let mut terminal = Terminal::new(4, 3, 0).unwrap();
    terminal.write("abc♥\u{fe0f}".as_bytes());
    for row in &terminal.primary.lines {
        for (x, cell) in row.cells.iter().enumerate() {
            if cell.wide == CellWide::Wide {
                assert_eq!(row.cells[x + 1].wide, CellWide::SpacerTail);
            }
            if cell.wide == CellWide::SpacerTail {
                assert!(x > 0);
                assert_eq!(row.cells[x - 1].wide, CellWide::Wide);
            }
        }
    }
}

#[test]
fn graphics_reflow_tracks_the_anchored_text_column() {
    let mut terminal = Terminal::new(10, 5, 100_000).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.write(b"abcdefgh\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2,C=1;/wAA/w==\x1b\\ijK");
    terminal.resize(5, 5, 0, 0).unwrap();
    assert_eq!(terminal.graphics.anchors(&terminal.primary)[0].1 .1, 3);
}

#[test]
fn status_string_queries_report_current_state_and_reject_unknown_requests() {
    let mut terminal = Terminal::new(10, 5, 0).unwrap();
    let replies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = replies.clone();
    terminal
        .set_write_pty_callback(move |bytes| sink.lock().unwrap().extend_from_slice(bytes))
        .unwrap();
    terminal.write(b"\x1b[2;4r\x1bP$qr\x1b\\\x1bP$qunknown\x1b\\");
    assert_eq!(
        *replies.lock().unwrap(),
        b"\x1bP1$r2;4r\x1b\\\x1bP0$r\x1b\\"
    );
}

#[test]
fn delete_lines_discards_text_without_history_or_moving_images() {
    let mut terminal = Terminal::new(10, 3, 100_000).unwrap();
    terminal.enable_kitty_graphics().unwrap();
    terminal.write(
        b"a\r\nb\r\nc\x1b[2;1H\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2,C=1;/wAA/w==\x1b\\",
    );
    let before = terminal.graphics.anchors(&terminal.primary);
    terminal.write(b"\x1b[H\x1b[M");
    assert_eq!(terminal.primary.history(), 0);
    assert_eq!(terminal.graphics.anchors(&terminal.primary), before);
    assert!(terminal
        .read_text_viewport((0, 0), (9, 2), false)
        .unwrap()
        .starts_with("b\nc"));
    terminal.write(b"\x1b[L");
    assert_eq!(terminal.graphics.anchors(&terminal.primary), before);
    terminal.write(b"\x1b[2J");
    assert!(terminal.kitty_image_placements().unwrap().is_empty());
    assert!(terminal.graphics.images.is_empty());
}

#[test]
fn ascii_word_writes_preserve_guards_styles_links_and_cell_layout() {
    use crate::screen::Cell;
    for (style, link) in [(0, 0), (1, 2), (u16::MAX, 0), (32768, u16::MAX)] {
        for len in 0..257 {
            let guard = Cell {
                scalar: ['界' as u32],
                style: 7,
                wide: CellWide::Wide,
                ..Cell::default()
            };
            let mut cells = vec![guard; len + 6];
            let bytes: Vec<_> = (0..len).map(|n| n as u8).collect();
            Cell::write_ascii(&mut cells[3..len + 3], &bytes, style, link);
            assert_eq!(cells[..3], [guard; 3]);
            assert_eq!(cells[len + 3..], [guard; 3]);
            for (cell, byte) in cells[3..len + 3].iter().zip(bytes) {
                assert_eq!(cell.scalar, [u32::from(byte)]);
                assert_eq!(cell.cluster, 0);
                assert_eq!((cell.style, cell.link), (style, link));
                assert_eq!(cell.wide, CellWide::Narrow);
            }
        }
    }
}

#[test]
fn bulk_parser_preserves_unicode_queries_and_graphics_at_every_split() {
    fn observe(chunks: &[&[u8]]) -> String {
        let mut terminal = Terminal::new(12, 4, 100_000).unwrap();
        terminal.enable_kitty_graphics().unwrap();
        let replies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = replies.clone();
        terminal
            .set_write_pty_callback(move |bytes| sink.lock().unwrap().extend_from_slice(bytes))
            .unwrap();
        for chunk in chunks {
            terminal.write(chunk);
        }
        format!(
            "{:?}|{:?}|{:?}|{:?}|{}|{:?}",
            terminal.screen_text_rows().unwrap(),
            terminal.take_pwd_changes(),
            *replies.lock().unwrap(),
            terminal.kitty_image_placements().unwrap(),
            terminal.take_bell_count(),
            terminal.mode_get(1000).unwrap()
        )
    }
    let input = concat!(
        "head\x1b[38:2::17:34:51m",
        "e\u{301}界👩\u{200d}💻",
        "\x1b[0m\x1b[?1000h\x1b]7;file:///tmp/herdr\x1b\\",
        "\x1bP+q544e\x1b\\\x1b[6n\x07",
        "\x1b_Ga=T,f=32,t=d,i=7,p=3,s=1,v=1,c=1,r=1,q=2,C=1;/wAA/w==\x1b\\",
        "\x1b_Gcancel\x18tail"
    )
    .as_bytes();
    let expected = observe(&[input]);
    for split in 0..=input.len() {
        assert_eq!(
            observe(&[&input[..split], &input[split..]]),
            expected,
            "split {split}"
        );
    }
    let bytes: Vec<_> = input.chunks(1).collect();
    assert_eq!(observe(&bytes), expected);
}
