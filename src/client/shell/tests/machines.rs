use super::super::machine_manager::{Hit, Page};
use super::*;
use crate::client::machine_setup::{Command, Event, Operation};
use crate::input::{TerminalKey, TextCommit};

fn shell() -> ClientShellState {
    let mut shell = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    shell.set_snapshot(Box::new(snapshot()));
    shell.set_pane_surface(surface());
    shell
}
fn open(shell: &mut ClientShellState) {
    let mut outcome = ClientShellInput::default();
    shell.open_add_server(&mut outcome);
    assert!(outcome.requests.is_empty());
    shell.machine_action(Hit::ManualEntry, &mut ClientShellInput::default());
}
fn press(shell: &mut ClientShellState, code: KeyCode) -> ClientShellInput {
    shell.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        code,
        KeyModifiers::empty(),
    ))])
}
fn profile(label: &str) -> SavedSshEndpoint {
    SavedSshEndpoint::new(label, "you@build.tailnet.ts.net", "main").unwrap()
}

fn external_session(pid: u32) -> crate::api::schema::ExternalTerminalSession {
    crate::api::schema::ExternalTerminalSession {
        identity: crate::api::schema::TerminalSessionIdentity {
            boot_id: "00000000-0000-0000-0000-000000000000".into(),
            uid: 1000,
            pid,
            start_ticks: 1234,
            session_id: pid,
            tty: 34817,
        },
        terminal: "/dev/pts/1".into(),
        command: "codex".into(),
        cwd: "/home/you/project".into(),
        process_count: 3,
    }
}

#[test]
fn sidebar_import_buttons_open_the_selected_server_without_activating_or_collapsing_it() {
    let servers = [profile("Build"), profile("Other")];
    for width in [18, 26, 36] {
        for status in [
            ClientEndpointStatus::Online,
            ClientEndpointStatus::Reconnecting,
            ClientEndpointStatus::Disabled,
        ] {
            for collapsed in [false, true] {
                let mut state = shell();
                state.sidebar_width = width;
                state.set_endpoint_catalog(&servers);
                let id = ClientEndpointId::Ssh(servers[1].id.clone());
                state.set_endpoint_status(&id, status);
                if collapsed {
                    state.collapsed_endpoints.insert(id.clone());
                }
                let active = state.active_endpoint_id.clone();
                state.compose(140, 48).unwrap();
                assert!(state
                    .hits
                    .machines
                    .iter()
                    .find(|hit| hit.endpoint_id.is_local())
                    .unwrap()
                    .import_sessions
                    .is_empty());
                let hit = state
                    .hits
                    .machines
                    .iter()
                    .find(|hit| hit.endpoint_id == id)
                    .unwrap();
                let button = hit.import_sessions;
                assert!(!button.is_empty());
                assert!(!button.intersects(hit.status_badge));
                assert!(!button.intersects(hit.collapse_toggle));
                let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: button.x,
                    row: button.y,
                    modifiers: KeyModifiers::empty(),
                })]);
                assert!(outcome.requests.is_empty());
                assert_eq!(state.active_endpoint_id, active);
                assert_eq!(state.collapsed_endpoints.contains(&id), collapsed);
                assert!(
                    matches!(&state.overlay, Some(ClientShellOverlay::Machines(manager)) if matches!(&manager.page, Page::Imports(profile) if profile.id == servers[1].id))
                );
                assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::MachineManager(Command::TerminalScan { profile, .. }) if profile.id == servers[1].id)));
                assert!(!outcome
                    .actions
                    .iter()
                    .any(|action| matches!(action, ClientShellAction::ActivateEndpoint { .. })));
            }
        }
    }
}

#[test]
fn terminal_import_captures_server_and_process_and_fences_late_results() {
    let mut state = shell();
    let server = profile("Build server");
    state.set_endpoint_catalog(std::slice::from_ref(&server));
    open(&mut state);
    if let Some(ClientShellOverlay::Machines(manager)) = &mut state.overlay {
        manager.page = Page::Manage(server.clone());
    }
    let mut outcome = ClientShellInput::default();
    state.machine_action(Hit::Imports, &mut outcome);
    let (id, request) = outcome
        .actions
        .iter()
        .find_map(|action| {
            if let ClientShellAction::MachineManager(Command::TerminalScan {
                id,
                request,
                profile,
            }) = action
            {
                assert_eq!(profile.target, server.target);
                Some((*id, *request))
            } else {
                None
            }
        })
        .unwrap();
    let report = crate::api::schema::TerminalSessionsReport {
        supported: true,
        helper_ready: true,
        message: "ready".into(),
        sessions: vec![external_session(123)],
    };
    assert!(!state.receive_machine_event(Event::TerminalScan {
        id,
        request: request + 1,
        profile: server.id.to_string(),
        result: Ok(report.clone())
    }));
    assert!(state.receive_machine_event(Event::TerminalScan {
        id,
        request,
        profile: server.id.to_string(),
        result: Ok(report)
    }));
    state.compose(110, 36).unwrap();
    let mut outcome = ClientShellInput::default();
    state.machine_action(Hit::ImportTerminal(0), &mut outcome);
    if let Some(ClientShellOverlay::Machines(manager)) = &mut state.overlay {
        let Page::Confirm {
            operation: Operation::ImportSession { profile, source },
            text,
        } = &manager.page
        else {
            panic!()
        };
        assert_eq!(profile.target, server.target);
        assert_eq!(source.pid, 123);
        assert!(text.contains("whole terminal moves"));
        manager.import_report.as_mut().unwrap().sessions = vec![external_session(999)];
    }
    state.compose(110, 36).unwrap();
    let outcome = press(&mut state, KeyCode::Enter);
    let (id, request) = outcome
        .actions
        .iter()
        .find_map(|action| {
            if let ClientShellAction::MachineManager(Command::TerminalImport {
                id,
                request,
                profile,
                source,
            }) = action
            {
                assert_eq!(source.pid, 123);
                assert_eq!(profile.target, server.target);
                Some((*id, *request))
            } else {
                None
            }
        })
        .unwrap();
    assert!(!state.receive_machine_event(Event::TerminalImported {
        id,
        request: request + 1,
        profile: server.id.to_string(),
        result: Ok("w2".into())
    }));
    assert!(state.receive_machine_event(Event::TerminalImported {
        id,
        request,
        profile: server.id.to_string(),
        result: Ok("w2".into())
    }));
    state.compose(110, 36).unwrap();
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(outcome.actions.iter().any(|action|matches!(action,ClientShellAction::ActivateEndpoint{endpoint_id:ClientEndpointId::Ssh(id),target:Some(ClientEndpointFocusTarget::Workspace(workspace))} if *id==server.id && workspace=="w2")));
}

#[test]
fn long_terminal_import_list_keeps_selected_source_visible() {
    let mut state = shell();
    open(&mut state);
    if let Some(ClientShellOverlay::Machines(manager)) = &mut state.overlay {
        manager.page = Page::Imports(profile("Build"));
        manager.import_report = Some(crate::api::schema::TerminalSessionsReport {
            supported: true,
            helper_ready: true,
            message: String::new(),
            sessions: (0..256).map(|n| external_session(100 + n)).collect(),
        });
        manager.selected = 256;
    }
    state.compose(100, 30).unwrap();
    assert!(state
        .hits
        .machine_actions
        .iter()
        .any(|(_, hit)| *hit == Hit::ImportTerminal(255)));
}

#[test]
fn add_defaults_to_automatic_key_setup_and_supports_control_k_opt_out() {
    for enabled in [true, false] {
        let mut state = shell();
        open(&mut state);
        state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("david@redie"))]);
        if !enabled {
            state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL,
            ))]);
        }
        let frame = state.compose(110, 36).unwrap();
        let text = frame
            .cells
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(text.contains("Automatic SSH key"));
        if enabled {
            assert!(text.contains("Enter your SSH password once"));
        }
        let outcome = press(&mut state, KeyCode::Enter);
        assert!(outcome.actions.iter().any(|action|matches!(action,
            ClientShellAction::MachineManager(Command::Run{operation:Operation::Add(form),..}) if form.setup_key==enabled
        )));
    }
}

#[test]
fn successful_key_repair_retries_attention_immediately_without_restarting_online_endpoints() {
    for (success, status, expected) in [
        (true, ClientEndpointStatus::Attention, true),
        (false, ClientEndpointStatus::Attention, false),
        (true, ClientEndpointStatus::Online, false),
    ] {
        let mut state = shell();
        let server = profile("Redie");
        state.set_endpoint_catalog(std::slice::from_ref(&server));
        let endpoint = ClientEndpointId::Ssh(server.id.clone());
        state.set_endpoint_status(&endpoint, status);
        open(&mut state);
        let id = if let Some(ClientShellOverlay::Machines(manager)) = &mut state.overlay {
            manager.profiles = vec![server.clone()];
            manager.last_operation = Some(Operation::SetupKey(server.id.to_string()));
            manager.id
        } else {
            panic!()
        };
        assert!(state.receive_machine_event(Event::Finished {
            id,
            success,
            message: "fixture".into()
        }));
        assert_eq!(
            state.take_machine_retries(),
            if expected { vec![endpoint] } else { vec![] }
        );
    }
}

#[test]
fn saved_server_update_uses_native_setup_without_sending_pane_input() {
    let mut state = shell();
    open(&mut state);
    let server = profile("Redie");
    if let Some(ClientShellOverlay::Machines(manager)) = &mut state.overlay {
        manager.page = Page::Manage(server.clone());
        manager.selected = 1;
    }
    let frame = state.compose(110, 36).unwrap();
    assert!(frame
        .cells
        .iter()
        .map(|c| c.symbol.as_str())
        .collect::<String>()
        .contains("Update Rust build"));
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(outcome.requests.is_empty());
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::MachineManager(Command::Run { operation: Operation::Update(id), .. }) if id == server.id.as_str()
    )));
}

#[test]
fn ssh_key_setup_reviews_the_captured_server_then_uses_native_auth() {
    let mut state = shell();
    open(&mut state);
    let server = profile("Redie");
    if let Some(ClientShellOverlay::Machines(manager)) = &mut state.overlay {
        manager.page = Page::Manage(server.clone());
        manager.selected = 2;
    }
    state.compose(110, 36).unwrap();
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(outcome.requests.is_empty());
    assert!(!outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::MachineManager(Command::Run { .. })
    )));
    if let Some(ClientShellOverlay::Machines(manager)) = &mut state.overlay {
        let Page::Confirm { text, .. } = &manager.page else {
            panic!()
        };
        assert!(text.contains("without a passphrase"));
        assert!(text.contains("Private keys are never synced"));
        manager.profiles = vec![profile("Changed list")];
    }
    state.compose(110, 36).unwrap();
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(outcome.requests.is_empty());
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::MachineManager(Command::Run { operation: Operation::SetupKey(id), .. }) if id == server.id.as_str()
    )));
}

#[test]
fn machine_add_is_available_local_only_collapsed_and_without_a_snapshot() {
    for unavailable in [false, true] {
        for collapsed in [false, true] {
            let mut state = shell();
            state.sidebar_collapsed = collapsed;
            if unavailable {
                state.snapshot = None;
                state.pane_surface = None;
            }
            state.compose(100, 30).unwrap();
            let button = state.hits.add_machine;
            assert!(
                button.width > 0,
                "unavailable={unavailable}, collapsed={collapsed}"
            );
            let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: button.x,
                row: button.y,
                modifiers: KeyModifiers::empty(),
            })]);
            assert!(outcome.requests.is_empty());
            assert!(matches!(
                state.overlay,
                Some(ClientShellOverlay::Machines(_))
            ));
            state.compose(100, 30).unwrap();
            assert!(!state.hits.machine_actions.is_empty());
        }
    }
}
#[test]
fn machine_form_input_and_native_auth_never_reach_the_remote_pane() {
    let mut state = shell();
    open(&mut state);
    let outcome = state.handle_raw_events(vec![
        RawInputEvent::Text(TextCommit::new("you@server")),
        RawInputEvent::Paste(".tailnet.ts.net".into()),
    ]);
    assert!(outcome.requests.is_empty());
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(outcome.requests.is_empty());
    let id = outcome
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::MachineManager(Command::Run {
                id,
                operation: Operation::Add(form),
                ..
            }) => {
                assert_eq!(form.target, "you@server.tailnet.ts.net");
                Some(*id)
            }
            _ => None,
        })
        .unwrap();
    let outcome = state.handle_raw_events(vec![
        RawInputEvent::Paste("private-password".into()),
        RawInputEvent::Key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty())),
    ]);
    assert!(outcome.requests.is_empty());
    assert!(outcome
        .actions
        .iter()
        .all(|a| matches!(a, ClientShellAction::MachineManager(_))));
    assert!(format!("{:?}", outcome.actions).contains("redacted"));
    assert!(!format!("{:?}", outcome.actions).contains("private-password"));
    let cancelled = state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        KeyCode::Char('g'),
        KeyModifiers::CONTROL,
    ))]);
    assert!(cancelled.actions.iter().any(|a|matches!(a,ClientShellAction::MachineManager(Command::Cancel{id:cancelled})if *cancelled==id)));
    assert!(!state.receive_machine_event(Event::Finished {
        id,
        success: true,
        message: "late".into()
    }));
}
#[test]
fn join_and_rename_tab_and_paste_always_edit_the_visible_field() {
    let mut state = shell();
    let p = profile("Before");
    state.set_endpoint_catalog(std::slice::from_ref(&p));
    let mut outcome = ClientShellInput::default();
    state.open_machine_context(&ClientEndpointId::Ssh(p.id.clone()), &mut outcome);
    state.machine_action(Hit::Rename, &mut outcome);
    press(&mut state, KeyCode::Tab);
    state.handle_raw_events(vec![RawInputEvent::Paste(" after".into())]);
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert_eq!(manager.fields[1].as_str(), "Before after");
    assert!(manager.fields[0].is_empty());
    state.machine_action(Hit::Join, &mut outcome);
    press(&mut state, KeyCode::Tab);
    state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("shared-host"))]);
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert_eq!(manager.fields[0].as_str(), "shared-host");
}
#[test]
fn machine_modal_paste_shortcut_uses_the_clipboard_path() {
    let mut state = shell();
    open(&mut state);
    let mut outcome = ClientShellInput::default();
    assert!(state.handle_modal_paste_shortcut_with(
        &TerminalKey::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
        &mut outcome,
        || Some("build-server".into())
    ));
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert_eq!(manager.fields[0].as_str(), "build-server");
    assert!(outcome.requests.is_empty());
}
#[test]
fn long_machine_and_sync_lists_scroll_selected_targets_into_view() {
    let mut state = shell();
    let profiles = (0..64)
        .map(|n| profile(&format!("Machine {n}")))
        .collect::<Vec<_>>();
    state.set_endpoint_catalog(&profiles);
    let mut outcome = ClientShellInput::default();
    state.open_machine_manager(&mut outcome);
    for _ in 0..65 {
        press(&mut state, KeyCode::Down);
    }
    state.compose(80, 22).unwrap();
    assert!(state
        .hits
        .machine_actions
        .iter()
        .any(|(_, hit)| *hit == Hit::Server(63)));
    press(&mut state, KeyCode::Enter);
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert!(matches!(&manager.page,Page::Manage(p)if p.id==profiles[63].id));
    state.machine_action(Hit::Sync, &mut outcome);
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    let target = super::super::machine_manager::menu_actions(manager)
        .iter()
        .position(|h| *h == Hit::UseServer(63))
        .unwrap();
    for _ in 0..target {
        press(&mut state, KeyCode::Down);
    }
    state.compose(80, 22).unwrap();
    assert!(state
        .hits
        .machine_actions
        .iter()
        .any(|(_, hit)| *hit == Hit::UseServer(63)));
}
#[test]
fn machine_confirmation_uses_the_captured_profile_and_requires_full_visibility() {
    let mut state = shell();
    let original = profile("Original");
    state.set_endpoint_catalog(std::slice::from_ref(&original));
    let mut outcome = ClientShellInput::default();
    state.open_machine_context(&ClientEndpointId::Ssh(original.id.clone()), &mut outcome);
    state.machine_action(Hit::Remove, &mut outcome);
    state.set_endpoint_catalog(&[profile("Replacement")]);
    state.compose(24, 10).unwrap();
    outcome = ClientShellInput::default();
    state.machine_action(Hit::Confirm, &mut outcome);
    assert!(!outcome
        .actions
        .iter()
        .any(|a| matches!(a, ClientShellAction::MachineManager(Command::Run { .. }))));
    state.compose(100, 30).unwrap();
    state.machine_action(Hit::Confirm, &mut outcome);
    assert!(outcome.actions.iter().any(|a|matches!(a,ClientShellAction::MachineManager(Command::Run{operation:Operation::Remove(id),..})if id==original.id.as_str())));
}
#[test]
fn machine_settings_and_suggestions_are_client_local_and_stale_events_are_ignored() {
    let mut state = shell();
    state.open_settings_overlay();
    let mut outcome = ClientShellInput::default();
    state.select_settings_section(ClientSettingsSection::Machines, &mut outcome);
    assert!(outcome.requests.is_empty());
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    let id = manager.id;
    assert!(!state.receive_machine_event(Event::Suggestions {
        id: id + 1,
        targets: vec!["wrong".into()],
        sync: "wrong".into()
    }));
    assert!(state.receive_machine_event(Event::Suggestions {
        id,
        targets: vec!["right".into()],
        sync: "Sync off".into()
    }));
    state.machine_action(Hit::Add, &mut outcome);
    state.machine_action(Hit::ManualEntry, &mut outcome);
    press(&mut state, KeyCode::Down);
    press(&mut state, KeyCode::Enter);
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert_eq!(manager.fields[0].as_str(), "right");
}
#[test]
fn machine_overlay_survives_remote_popup_and_projection_changes() {
    let mut state = shell();
    open(&mut state);
    state.set_pane_surface(surface_with_popup());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Machines(_))
    ));
    state.reset_endpoint_projection();
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Machines(_))
    ));
}
#[test]
fn machine_dialogs_remain_modal_at_tiny_sizes_and_resize_the_owned_terminal() {
    let mut state = shell();
    open(&mut state);
    for (width, height) in [(8, 6), (24, 10), (80, 24), (140, 50)] {
        assert!(state.compose(width, height).is_some());
    }
    state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("host"))]);
    press(&mut state, KeyCode::Enter);
    let resize = state.machine_resize(80, 24).unwrap();
    assert!(matches!(
        resize,
        Command::Resize {
            cols: 74,
            rows: 14,
            ..
        }
    ));
}

#[test]
fn machine_install_approval_uses_explicit_private_answers_and_defaults_to_decline() {
    let mut state = shell();
    open(&mut state);
    state.compose(100, 30).unwrap();
    let id = if let Some(ClientShellOverlay::Machines(m)) = &mut state.overlay {
        m.page = Page::Running;
        m.selected = 5;
        m.id
    } else {
        unreachable!()
    };
    assert!(state.receive_machine_event(Event::Approval {
        id,
        prompt: crate::remote::setup_progress::Approval {
            request: 8,
            title: "Install Herdr".into(),
            detail: "Install package on build server?".into(),
            restart: false
        }
    }));
    let paste = state.handle_raw_events(vec![RawInputEvent::Paste("yes\n".into())]);
    assert!(paste.requests.is_empty());
    assert!(paste.actions.is_empty());
    state.compose(100, 30).unwrap();
    let answer = press(&mut state, KeyCode::Enter);
    assert!(answer.requests.is_empty());
    assert!(
        matches!(answer.actions.as_slice(),[ClientShellAction::MachineManager(Command::Approval{id:job,request:8,accepted:false})] if *job==id)
    );
    let mut outcome = ClientShellInput::default();
    state.machine_action(Hit::Approve, &mut outcome);
    assert!(outcome.actions.is_empty());
    assert!(!state.receive_machine_event(Event::Approval {
        id: id + 100,
        prompt: crate::remote::setup_progress::Approval {
            request: 9,
            title: "stale".into(),
            detail: "stale".into(),
            restart: false
        }
    }));
}

#[test]
fn machine_dialogs_render_controls_and_optional_review_frames() {
    let mut state = shell();
    open(&mut state);
    let variants = [
        "add",
        "manage",
        "sync",
        "approval",
        "tailscale",
        "tailscale-missing",
        "manager-sync",
        "manager-consent",
    ];
    for name in variants {
        if let Some(ClientShellOverlay::Machines(m)) = &mut state.overlay {
            m.profiles = vec![profile("Build server")];
            m.page = match name {
                "add" => { m.advanced=true; m.fields[0]="you@build.tailnet.ts.net".into(); Page::Add }
                "manage" => Page::Manage(profile("Build server")),
                "sync" => { m.sync="Shared list: you@build.tailnet.ts.net · synced just now".into(); Page::Sync }
                "tailscale" => {
                    let mut device=tail_device("Build server",Some(true),Some("build.tailnet.ts.net"),"100.90.0.2");device.ips.push("fd7a:115c:a1e0::2".parse().unwrap());
                    m.tailscale.devices=vec![device,tail_device("Laptop",Some(false),Some("laptop.tailnet.ts.net"),"100.90.0.8")];
                    Page::Tailscale{join:false}
                }
                "tailscale-missing" => { m.tailscale.devices.clear();m.tailscale.error=Some("Tailscale was not found. Install or open Tailscale, then Refresh; or enter a host manually.".into());Page::Tailscale{join:false} }
                "manager-sync"=>{
                    m.direct=crate::client::direct_sync::View{listening:true,incoming:vec![sync_request(&"2".repeat(64))],managers:vec![sync_peer_manager()],..Default::default()};Page::Managers
                },
                "manager-consent"=>{let (action,text)=super::super::machine_manager::manager_sync::consent(m,Hit::ApprovePair(0)).unwrap();Page::DirectConfirm{action,text}},
                _ => Page::Approval(crate::remote::setup_progress::Approval{request:1,title:"Install Herdr".into(),detail:"Install the local Herdr binary on you@build.tailnet.ts.net at ~/.local/bin/herdr?".into(),restart:false}),
            };
            m.selected = 0;
        }
        let frame = state.compose(110, 36).unwrap();
        let text = frame
            .cells
            .iter()
            .map(|c| c.symbol.as_str())
            .collect::<String>();
        assert!(
            text.contains(match name {
                "add" => "Key file (local)",
                "manage" => "Reconnect / authenticate",
                "sync" => "Sync now",
                "tailscale" => "100.90.0.2",
                "tailscale-missing" => "Tailscale was not found",
                "manager-sync" => "Approve code",
                "manager-consent" => "Compare code",
                _ => "Decline",
            }),
            "{name}: {text}"
        );
        if let Some(root) = std::env::var_os("HERDR_TEST_MACHINE_PREVIEWS") {
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                std::path::PathBuf::from(root).join(format!("{name}.json")),
                serde_json::to_vec(&frame.frame).unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
fn add_server_username_is_visible_editable_and_used_for_connect() {
    let mut state = shell();
    open(&mut state);
    let frame = state.compose(100, 30).unwrap();
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("Host / IP:"));
    assert!(text.contains("Username:"));
    assert!(text.contains("Enter your SSH password once; future logins use a dedicated key."));
    assert!(text.contains("Password prompts appear here after Connect (input is hidden)."));
    let outcome = state.handle_raw_events(vec![
        RawInputEvent::Paste("192.168.1.50".into()),
        RawInputEvent::Key(TerminalKey::new(KeyCode::Tab, KeyModifiers::empty())),
        RawInputEvent::Paste("david".into()),
        RawInputEvent::Key(TerminalKey::new(KeyCode::Tab, KeyModifiers::empty())),
        RawInputEvent::Paste("Build server".into()),
        RawInputEvent::Key(TerminalKey::new(KeyCode::Enter, KeyModifiers::empty())),
    ]);
    assert!(outcome.requests.is_empty());
    let [ClientShellAction::MachineManager(Command::Run {
        operation: Operation::Add(form),
        ..
    })] = outcome.actions.as_slice()
    else {
        panic!("{:?}", outcome.actions)
    };
    assert_eq!(form.username, "david");
    assert_eq!(form.label, "Build server");
    assert_eq!(form.destination().unwrap(), "david@192.168.1.50");
}

#[test]
fn username_and_auth_hints_keep_keyboard_suggestions_visible_in_short_forms() {
    let mut state = shell();
    open(&mut state);
    if let Some(ClientShellOverlay::Machines(m)) = &mut state.overlay {
        m.advanced = true;
        m.suggestions = (0..12).map(|n| format!("host-{n}")).collect();
    }
    state.compose(80, 24).unwrap();
    for _ in 0..6 {
        press(&mut state, KeyCode::Down);
    }
    state.compose(80, 24).unwrap();
    assert!(state
        .hits
        .machine_actions
        .iter()
        .any(|(_, h)| *h == Hit::Suggestion(5)));
}

fn tail_device(
    name: &str,
    online: Option<bool>,
    dns: Option<&str>,
    ip: &str,
) -> crate::client::machine_setup::TailscaleDevice {
    crate::client::machine_setup::TailscaleDevice {
        id: None,
        name: name.into(),
        online,
        dns: dns.map(str::to_owned),
        ips: vec![ip.parse().unwrap()],
    }
}
fn refresh_tailnet(state: &mut ClientShellState) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    ))])
}
fn browse(state: &mut ClientShellState) -> (u64, u64) {
    let outcome = state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        KeyCode::Char('t'),
        KeyModifiers::ALT,
    ))]);
    assert!(outcome.requests.is_empty());
    let [ClientShellAction::MachineManager(Command::Tailscale { id, request })] =
        outcome.actions.as_slice()
    else {
        panic!("{:?}", outcome.actions)
    };
    (*id, *request)
}
#[test]
fn tailscale_browser_selects_dns_or_ip_without_connecting_and_preserves_form() {
    for dns in [Some("build.tailnet.ts.net"), None] {
        let mut state = shell();
        open(&mut state);
        state.compose(100, 30).unwrap();
        if let Some(ClientShellOverlay::Machines(m)) = &mut state.overlay {
            m.fields[1] = "My build".into();
            m.fields[5] = "david".into();
        }
        assert!(state
            .hits
            .machine_actions
            .iter()
            .any(|(_, hit)| *hit == Hit::BrowseTailscale));
        let (id, request) = browse(&mut state);
        let mut device = tail_device("Build", Some(true), dns, "100.90.0.2");
        device.ips.push("fd7a:115c:a1e0::2".parse().unwrap());
        assert!(state.receive_machine_event(Event::Tailscale {
            id,
            request,
            result: Ok(vec![device])
        }));
        let frame = state.compose(100, 30).unwrap();
        let text = frame
            .cells
            .iter()
            .map(|c| c.symbol.as_str())
            .collect::<String>();
        assert!(text.contains("online"));
        assert!(text.contains("100.90.0.2"));
        assert!(text.contains("fd7a:115c:a1e0::2"));
        let outcome = press(&mut state, KeyCode::Enter);
        assert!(outcome.actions.is_empty());
        assert!(outcome.requests.is_empty());
        let Some(ClientShellOverlay::Machines(m)) = &state.overlay else {
            panic!()
        };
        assert!(matches!(m.page, Page::Add));
        assert_eq!(m.focus, 5);
        assert_eq!(m.fields[0].as_str(), dns.unwrap_or("100.90.0.2"));
        assert_eq!(m.fields[1].as_str(), "My build");
        assert_eq!(m.fields[5].as_str(), "david");
    }
}
#[test]
fn tailscale_refresh_rejects_stale_replies_and_back_allows_a_new_query() {
    let mut state = shell();
    open(&mut state);
    let (id, request) = browse(&mut state);
    let duplicate = refresh_tailnet(&mut state);
    assert!(duplicate.actions.is_empty());
    assert!(!state.receive_machine_event(Event::Tailscale {
        id,
        request: request + 1,
        result: Ok(vec![])
    }));
    assert!(state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Err("Tailscale is disconnected. Connect, then Refresh.".into())
    }));
    let frame = state.compose(80, 22).unwrap();
    let text = frame
        .cells
        .iter()
        .map(|c| c.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("disconnected"));
    let refreshed = refresh_tailnet(&mut state);
    let [ClientShellAction::MachineManager(Command::Tailscale {
        id: new_id,
        request: new_request,
    })] = refreshed.actions.as_slice()
    else {
        panic!()
    };
    assert_eq!(*new_id, id);
    assert_ne!(*new_request, request);
    assert!(!state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![])
    }));
    press(&mut state, KeyCode::Esc);
    let (next_id, next_request) = browse(&mut state);
    assert_ne!(next_id, id);
    assert_ne!(next_request, request);
    assert!(!state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![])
    }));
}
#[test]
fn tailscale_large_list_scrolls_selection_and_refresh_preserves_device_identity() {
    let mut state = shell();
    open(&mut state);
    let (id, request) = browse(&mut state);
    let devices = (0..256)
        .map(|n| {
            tail_device(
                &format!("Device {n}"),
                Some(true),
                None,
                &format!("100.90.0.{n}"),
            )
        })
        .collect::<Vec<_>>();
    assert!(state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(devices.clone())
    }));
    for _ in 0..255 {
        press(&mut state, KeyCode::Down);
    }
    state.compose(80, 22).unwrap();
    assert!(state
        .hits
        .machine_actions
        .iter()
        .any(|(_, hit)| *hit == Hit::TailscaleDevice(255)));
    let refreshed = refresh_tailnet(&mut state);
    let [ClientShellAction::MachineManager(Command::Tailscale { request, .. })] =
        refreshed.actions.as_slice()
    else {
        panic!()
    };
    let mut reversed = devices;
    reversed.reverse();
    assert!(state.receive_machine_event(Event::Tailscale {
        id,
        request: *request,
        result: Ok(reversed)
    }));
    let Some(ClientShellOverlay::Machines(m)) = &state.overlay else {
        panic!()
    };
    assert_eq!(m.selected, 0);
    assert_eq!(m.tailscale.devices[0].name, "Device 255");
}
#[test]
fn tailscale_mouse_selection_and_join_preserve_the_ssh_user() {
    let mut state = shell();
    open(&mut state);
    let mut outcome = ClientShellInput::default();
    state.machine_action(Hit::Join, &mut outcome);
    if let Some(ClientShellOverlay::Machines(m)) = &mut state.overlay {
        m.fields[0] = "you@previous-host".into();
    }
    let (id, request) = browse(&mut state);
    assert!(state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![tail_device("Laptop", Some(true), None, "100.90.0.8")])
    }));
    state.compose(100, 30).unwrap();
    let rect = state
        .hits
        .machine_actions
        .iter()
        .find_map(|(r, h)| (*h == Hit::TailscaleDevice(0)).then_some(*r))
        .unwrap();
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(outcome.actions.is_empty());
    assert!(outcome.requests.is_empty());
    let Some(ClientShellOverlay::Machines(m)) = &state.overlay else {
        panic!()
    };
    assert!(matches!(m.page, Page::Join));
    assert_eq!(m.fields[0].as_str(), "you@100.90.0.8");
}

#[test]
fn tailscale_pending_refresh_cannot_block_discovery_after_connect_cancellation() {
    let mut state = shell();
    open(&mut state);
    state.compose(100, 30).unwrap();
    let (id, request) = browse(&mut state);
    assert!(state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![tail_device("Build", Some(true), None, "100.90.0.2")])
    }));
    refresh_tailnet(&mut state);
    press(&mut state, KeyCode::Enter);
    let mut outcome = ClientShellInput::default();
    state.machine_action(Hit::Connect, &mut outcome);
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::MachineManager(Command::Run { .. })
    )));
    state.machine_action(Hit::Back, &mut outcome);
    let (next_id, next_request) = browse(&mut state);
    assert_ne!(next_id, id);
    assert_ne!(next_request, request);
    assert!(!state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![])
    }));
}

#[test]
fn tailscale_control_t_works_from_raw_terminal_bytes_and_stays_inside_forms() {
    for join in [false, true] {
        let mut state = shell();
        open(&mut state);
        if join {
            let mut outcome = ClientShellInput::default();
            state.machine_action(Hit::Join, &mut outcome);
        }
        let frame = state.compose(100, 30).unwrap();
        let text = frame
            .cells
            .iter()
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(text.contains("Ctrl+T"));
        let opened = state.handle_input_bytes(b"\x14");
        assert!(opened.requests.is_empty());
        assert!(matches!(
            opened.actions.as_slice(),
            [ClientShellAction::MachineManager(Command::Tailscale { .. })]
        ));
        let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
            panic!()
        };
        assert!(matches!(manager.page,Page::Tailscale{join:source} if source==join));
        press(&mut state, KeyCode::Esc);
        if let Some(ClientShellOverlay::Machines(manager)) = &mut state.overlay {
            manager.page = Page::Running;
        }
        let native = state.handle_input_bytes(b"\x14");
        assert!(native.requests.is_empty());
        assert!(
            matches!(native.actions.as_slice(),[ClientShellAction::MachineManager(Command::Input{bytes,..})] if bytes==b"\x14")
        );
    }
}

#[test]
fn tailscale_search_hides_offline_devices_and_typing_selects_the_correct_match() {
    let mut state = shell();
    open(&mut state);
    let (id, request) = browse(&mut state);
    assert!(state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![
            tail_device(
                "Alpha",
                Some(true),
                Some("alpha.tailnet.ts.net"),
                "100.90.0.1"
            ),
            tail_device("Offline Remote", Some(false), None, "100.90.0.9"),
            tail_device(
                "Remote Build",
                Some(true),
                Some("build.tailnet.ts.net"),
                "100.90.0.2"
            ),
            tail_device("Unknown Remote", None, None, "100.90.0.10"),
        ])
    }));
    let frame = state.compose(100, 30).unwrap();
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(!text.contains("Offline Remote"));
    assert!(!text.contains("Unknown Remote"));
    assert!(!text.contains("100.90.0.9"));
    let typed = state.handle_input_bytes(b"REMOTE");
    assert!(typed.actions.is_empty());
    assert!(typed.requests.is_empty());
    let frame = state.compose(100, 30).unwrap();
    assert!(frame.cursor.as_ref().is_some_and(|cursor| cursor.visible));
    assert!(state
        .hits
        .machine_actions
        .iter()
        .any(|(_, hit)| *hit == Hit::TailscaleDevice(1)));
    assert!(!state
        .hits
        .machine_actions
        .iter()
        .any(|(_, hit)| *hit == Hit::TailscaleDevice(0)));
    let mut stale = ClientShellInput::default();
    state.machine_action(Hit::TailscaleDevice(0), &mut stale);
    assert!(stale.actions.is_empty());
    assert!(
        matches!(&state.overlay,Some(ClientShellOverlay::Machines(m)) if matches!(m.page,Page::Tailscale{..}))
    );
    let selected = press(&mut state, KeyCode::Enter);
    assert!(selected.actions.is_empty());
    let Some(ClientShellOverlay::Machines(m)) = &state.overlay else {
        panic!()
    };
    assert_eq!(m.fields[0].as_str(), "build.tailnet.ts.net");
    assert_eq!(m.focus, 5);
}
#[test]
fn tailscale_search_accepts_paste_dns_and_ipv6_and_stays_active_after_refresh() {
    let mut state = shell();
    open(&mut state);
    let (id, request) = browse(&mut state);
    let mut device = tail_device(
        "Build",
        Some(true),
        Some("build.tailnet.ts.net"),
        "100.90.0.2",
    );
    device.ips.push("fd7a:115c:a1e0::2".parse().unwrap());
    assert!(state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![device.clone()])
    }));
    let pasted = state.handle_raw_events(vec![RawInputEvent::Paste("FD7A:115C".into())]);
    assert!(pasted.actions.is_empty());
    assert!(pasted.requests.is_empty());
    state.compose(100, 30).unwrap();
    assert!(state
        .hits
        .machine_actions
        .iter()
        .any(|(_, hit)| *hit == Hit::TailscaleDevice(0)));
    let clear = state.handle_input_bytes(b"\x15");
    assert!(clear.actions.is_empty());
    state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("BUILD.TAILNET"))]);
    let refreshed = state.handle_input_bytes(b"\x12");
    let [ClientShellAction::MachineManager(Command::Tailscale { request, .. })] =
        refreshed.actions.as_slice()
    else {
        panic!("{:?}", refreshed.actions)
    };
    device.online = Some(false);
    assert!(state.receive_machine_event(Event::Tailscale {
        id,
        request: *request,
        result: Ok(vec![
            device,
            tail_device("Other", Some(true), None, "100.90.0.3")
        ])
    }));
    let frame = state.compose(100, 30).unwrap();
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("No online devices match"));
    let Some(ClientShellOverlay::Machines(m)) = &state.overlay else {
        panic!()
    };
    assert_eq!(m.tailscale.query.as_str(), "BUILD.TAILNET");
    assert_eq!(m.tailscale.devices.len(), 1);
    state.handle_input_bytes(b"\x15");
    state.compose(100, 30).unwrap();
    assert!(state
        .hits
        .machine_actions
        .iter()
        .any(|(_, hit)| *hit == Hit::TailscaleDevice(0)));
    let mut outcome = ClientShellInput::default();
    assert!(state.handle_modal_paste_shortcut_with(
        &TerminalKey::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
        &mut outcome,
        || Some("Other".into())
    ));
    assert!(outcome.requests.is_empty());
    let Some(ClientShellOverlay::Machines(m)) = &state.overlay else {
        panic!()
    };
    assert_eq!(m.tailscale.query.as_str(), "Other");
    assert!(m.fields[0].is_empty());
}

fn sync_peer_manager() -> crate::client::direct_sync::Manager {
    crate::client::direct_sync::Manager {
        id: "1".repeat(32),
        node: "peer-node".into(),
        name: "Laptop".into(),
        ip: "100.90.0.2".parse().unwrap(),
    }
}
fn sync_request(request: &str) -> crate::client::direct_sync::PairRequest {
    crate::client::direct_sync::PairRequest {
        request: request.into(),
        manager: sync_peer_manager(),
        code: "1234-5678-9abc-def0-1234-5678-9abc-def0".into(),
        expires: u64::MAX,
        accepted: false,
    }
}
#[test]
fn direct_sync_background_metadata_does_not_trigger_presentation_but_incoming_requests_do() {
    let mut state = shell();
    let mut view = crate::client::direct_sync::View {
        listening: true,
        paired: vec![crate::client::direct_sync::Paired {
            manager: sync_peer_manager(),
            last_success: Some(1),
            error: None,
        }],
        ..Default::default()
    };
    assert!(!state.receive_direct_sync(view.clone()));
    view.paired[0].last_success = Some(2);
    assert!(!state.receive_direct_sync(view.clone()));
    view.incoming.push(sync_request(&"2".repeat(64)));
    assert!(state.receive_direct_sync(view.clone()));
    assert!(state.overlay.is_none());
    assert!(state.visible_endpoint_notice.is_some());
    assert!(!state.receive_direct_sync(view));
}
#[test]
fn direct_sync_consent_defaults_to_back_and_preserves_captured_request_after_refresh() {
    let mut state = shell();
    open(&mut state);
    state.receive_direct_sync(crate::client::direct_sync::View {
        incoming: vec![sync_request(&"2".repeat(64))],
        ..Default::default()
    });
    let mut outcome = ClientShellInput::default();
    state.machine_action(Hit::ManagerSync, &mut outcome);
    state.compose(100, 30).unwrap();
    state.machine_action(Hit::ApprovePair(0), &mut outcome);
    let safe = press(&mut state, KeyCode::Enter);
    assert!(!safe.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::MachineManager(Command::Direct { .. })
    )));
    state.machine_action(Hit::ApprovePair(0), &mut outcome);
    state.receive_direct_sync(crate::client::direct_sync::View {
        incoming: vec![sync_request(&"3".repeat(64))],
        ..Default::default()
    });
    let mut approved = ClientShellInput::default();
    state.machine_action(Hit::Confirm, &mut approved);
    assert!(
        matches!(approved.actions.as_slice(),[ClientShellAction::MachineManager(Command::Direct{action:crate::client::direct_sync::Action::Approve{request,accepted:true},..})] if request==&"2".repeat(64))
    );
    assert!(approved.requests.is_empty());
}
#[test]
fn direct_sync_requests_do_not_interrupt_native_authentication_and_small_windows_cannot_approve() {
    let mut state = shell();
    open(&mut state);
    if let Some(ClientShellOverlay::Machines(m)) = &mut state.overlay {
        m.page = Page::Running;
    }
    assert!(state.receive_direct_sync(crate::client::direct_sync::View {
        incoming: vec![sync_request(&"2".repeat(64))],
        ..Default::default()
    }));
    assert!(
        matches!(&state.overlay,Some(ClientShellOverlay::Machines(m))if matches!(m.page,Page::Running))
    );
    let typed = state.handle_input_bytes(b"password");
    assert!(typed.requests.is_empty());
    assert!(typed.actions.iter().all(|action| matches!(
        action,
        ClientShellAction::MachineManager(Command::Input { .. })
    )));
    let mut outcome = ClientShellInput::default();
    state.machine_action(Hit::ManagerSync, &mut outcome);
    state.machine_action(Hit::ApprovePair(0), &mut outcome);
    state.compose(32, 10).unwrap();
    let mut denied = ClientShellInput::default();
    state.machine_action(Hit::Confirm, &mut denied);
    assert!(denied.actions.is_empty());
}

#[test]
fn sync_badge_is_clickable_without_covering_sidebar_controls_or_resizing_panes() {
    for collapsed in [false, true] {
        let mut state = shell();
        state.sidebar_collapsed = collapsed;
        let before = state.surface_size(140, 48);
        state.compose(140, 48).unwrap();
        let badge = state.hits.sync_status;
        assert!(!badge.is_empty());
        assert!(!badge.intersects(state.hits.add_machine));
        assert!(!badge.intersects(state.hits.sidebar_toggle));
        assert!(!badge.intersects(state.hits.workspace_body));
        assert!(!badge.intersects(state.hits.agent_body));
        assert_eq!(before, state.surface_size(140, 48));
        let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: badge.x,
            row: badge.bottom() - 1,
            modifiers: KeyModifiers::empty(),
        })]);
        assert!(outcome.repaint);
        assert!(
            matches!(&state.overlay,Some(ClientShellOverlay::Machines(m)) if matches!(m.page,Page::Sync))
        );
    }
}
#[test]
fn sync_details_update_without_repainting_closed_modal_and_preserve_selected_action() {
    let mut state = shell();
    let mut view = crate::client::sync_status::View::default();
    state.receive_sync_status(view.clone());
    view.details = "Updated age".into();
    assert!(!state.receive_sync_status(view.clone()));
    view.updates = "Updates: 2 current · auto".into();
    view.update_level = crate::client::sync_status::Level::Good;
    assert!(state.receive_sync_status(view.clone()));
    state.open_sync_status(&mut ClientShellInput::default());
    if let Some(ClientShellOverlay::Machines(m)) = state.overlay.as_mut() {
        m.selected = super::super::machine_manager::menu_actions(m)
            .iter()
            .position(|h| *h == Hit::RefreshSync)
            .unwrap();
    }
    view.details = "More\nmanager\nstatus\nlines".into();
    assert!(state.receive_sync_status(view));
    let Some(ClientShellOverlay::Machines(m)) = &state.overlay else {
        panic!()
    };
    assert_eq!(
        super::super::machine_manager::menu_actions(m)[m.selected],
        Hit::RefreshSync
    );
    let result = press(&mut state, KeyCode::Enter);
    assert!(result.actions.iter().any(|a| matches!(
        a,
        ClientShellAction::MachineManager(Command::RefreshStatus { .. })
    )));
}
#[test]
fn sync_badge_is_present_without_active_surface_and_disabled_when_mouse_is_off() {
    let mut state = shell();
    state.select_unavailable_local();
    state.compose(140, 48).unwrap();
    assert!(!state.hits.sync_status.is_empty());
    state.config.mouse_capture = false;
    state.compose(140, 48).unwrap();
    assert!(state.hits.sync_status.is_empty());
}

#[test]
fn automatic_update_waits_for_dialogs_and_navigation_to_finish() {
    let mut state = shell();
    assert!(state.automatic_update_ready());
    open(&mut state);
    assert!(!state.automatic_update_ready());
    state.overlay = None;
    state.mode = ClientShellMode::Navigate;
    assert!(!state.automatic_update_ready());
    state.mode = ClientShellMode::Terminal;
    assert!(state.automatic_update_ready());
}

fn quick_add(state: &mut ClientShellState) -> (u64, u64) {
    let mut outcome = ClientShellInput::default();
    state.open_add_server(&mut outcome);
    let [ClientShellAction::MachineManager(Command::Tailscale { id, request })] =
        outcome.actions.as_slice()
    else {
        panic!("discovery must not connect: {:?}", outcome.actions)
    };
    (*id, *request)
}
#[test]
fn quick_add_discovers_immediately_and_one_selection_prefills_and_starts_setup() {
    let mut state = shell();
    state.machine_profiles = vec![profile("Existing")];
    let (id, request) = quick_add(&mut state);
    assert!(!state.receive_machine_event(Event::Tailscale {
        id,
        request: request + 1,
        result: Ok(vec![])
    }));
    assert!(state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![
            tail_device(
                "New build",
                Some(true),
                Some("new.tailnet.ts.net"),
                "100.90.0.3"
            ),
            tail_device("Offline", Some(false), None, "100.90.0.4")
        ])
    }));
    state.compose(100, 36).unwrap();
    assert!(state
        .hits
        .machine_actions
        .iter()
        .any(|(_, h)| *h == Hit::ManualEntry));
    let outcome = press(&mut state, KeyCode::Enter);
    let [ClientShellAction::MachineManager(Command::Run {
        operation: Operation::Add(form),
        ..
    })] = outcome.actions.as_slice()
    else {
        panic!("{:?}", outcome.actions)
    };
    assert_eq!(form.target, "new.tailnet.ts.net");
    assert_eq!(form.username, "you");
    assert_eq!(form.label, "New build");
    assert!(form.setup_key);
    assert_eq!(form.destination().unwrap(), "you@new.tailnet.ts.net");
    assert!(!state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![])
    }));
}
#[test]
fn quick_add_username_edit_is_separate_from_search_and_manual_entry_preserves_it() {
    let mut state = shell();
    let (id, request) = quick_add(&mut state);
    state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![tail_device("Build", Some(true), None, "100.90.0.2")]),
    });
    press(&mut state, KeyCode::Tab);
    state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new("bob"))]);
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(outcome.actions.is_empty());
    let Some(ClientShellOverlay::Machines(m)) = &state.overlay else {
        panic!()
    };
    assert_eq!(m.fields[5].as_str(), "bob");
    assert!(m.tailscale.query.is_empty());
    assert_eq!(m.focus, 0);
    state.machine_action(Hit::ManualEntry, &mut ClientShellInput::default());
    let Some(ClientShellOverlay::Machines(m)) = &state.overlay else {
        panic!()
    };
    assert!(matches!(m.page, Page::Add));
    assert!(!m.quick_add);
    assert_eq!(m.fields[5].as_str(), "bob");
}
#[test]
fn quick_add_reuses_matching_saved_profile_but_never_overrides_edited_username() {
    for username in ["you", "bob", ""] {
        let mut state = shell();
        let saved = profile("Saved build");
        state.machine_profiles = vec![saved.clone()];
        let (id, request) = quick_add(&mut state);
        state.receive_machine_event(Event::Tailscale {
            id,
            request,
            result: Ok(vec![tail_device(
                "Build",
                Some(true),
                Some("build.tailnet.ts.net"),
                "100.90.0.2",
            )]),
        });
        if let Some(ClientShellOverlay::Machines(m)) = &mut state.overlay {
            m.fields[5] = username.into();
        }
        let outcome = press(&mut state, KeyCode::Enter);
        let [ClientShellAction::MachineManager(Command::Run { operation, .. })] =
            outcome.actions.as_slice()
        else {
            panic!("{:?}", outcome.actions)
        };
        if username == "you" {
            assert!(matches!(operation,Operation::Reconnect(id) if id==saved.id.as_str()));
        } else {
            let Operation::Add(form) = operation else {
                panic!()
            };
            assert_eq!(form.username, username);
            assert_eq!(
                form.destination().unwrap(),
                if username.is_empty() {
                    "build.tailnet.ts.net"
                } else {
                    "bob@build.tailnet.ts.net"
                }
            );
        }
    }
}
#[test]
fn checkin_only_updates_repaint_visible_cards_but_not_collapsed_summary() {
    for mouse in [true, false] {
        let mut state = shell();
        state.config.mouse_capture = mouse;
        let mut view = crate::client::sync_status::View::default();
        view.managers.push(crate::client::sync_status::ManagerRow {
            name: "Laptop".into(),
            status: "✓ v0.9.5 b3 · auto on".into(),
            compact: "b3 auto on".into(),
            last_seen: "Seen 5s ago".into(),
            level: crate::client::sync_status::Level::Good,
        });
        state.receive_sync_status(view.clone());
        state.compose(140, 48).unwrap();
        view.managers[0].last_seen = "Seen 10s ago".into();
        assert!(state.receive_sync_status(view.clone()));
        state.sidebar_collapsed = true;
        state.compose(140, 48).unwrap();
        view.managers[0].last_seen = "Seen 15s ago".into();
        assert!(!state.receive_sync_status(view));
    }
}

#[test]
fn quick_add_rejects_stale_mouse_hits_after_a_device_list_refresh() {
    let mut state = shell();
    let (id, request) = quick_add(&mut state);
    state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(vec![tail_device("First", Some(true), None, "100.90.0.2")]),
    });
    state.compose(100, 36).unwrap();
    let hit = state
        .hits
        .machine_actions
        .iter()
        .find_map(|(_, h)| matches!(h, Hit::ToggleDevice(..)).then_some(*h))
        .unwrap();
    let refresh = refresh_tailnet(&mut state);
    let [ClientShellAction::MachineManager(Command::Tailscale { request, .. })] =
        refresh.actions.as_slice()
    else {
        panic!()
    };
    state.receive_machine_event(Event::Tailscale {
        id,
        request: *request,
        result: Ok(vec![tail_device("Second", Some(true), None, "100.90.0.3")]),
    });
    let mut stale = ClientShellInput::default();
    state.machine_action(hit, &mut stale);
    assert!(stale.actions.is_empty());
    let outcome = press(&mut state, KeyCode::Enter);
    assert!(
        matches!(outcome.actions.as_slice(),[ClientShellAction::MachineManager(Command::Run{operation:Operation::Add(form),..})] if form.target=="100.90.0.3")
    );
}

fn batch_devices() -> Vec<crate::client::machine_setup::TailscaleDevice> {
    (1..=3)
        .map(|n| {
            let mut device = tail_device(
                &format!("Build {n}"),
                Some(true),
                None,
                &format!("100.90.0.{n}"),
            );
            device.id = Some(format!("node-{n}"));
            device
        })
        .collect()
}
fn select_devices(
    state: &mut ClientShellState,
    devices: Vec<crate::client::machine_setup::TailscaleDevice>,
) {
    let (id, request) = quick_add(state);
    assert!(state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(devices.clone())
    }));
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    let revision = manager.tailscale.revision;
    for n in 0..devices.len() {
        let mut outcome = ClientShellInput::default();
        state.machine_action(Hit::ToggleDevice(n, revision), &mut outcome);
        assert!(outcome.actions.is_empty());
    }
}
fn run_id(outcome: &ClientShellInput) -> u64 {
    let [ClientShellAction::MachineManager(Command::Run { id, .. })] = outcome.actions.as_slice()
    else {
        panic!("{:?}", outcome.actions)
    };
    *id
}
#[test]
fn batch_selection_survives_search_refresh_and_rejects_replacement_node_at_same_address() {
    let mut state = shell();
    let devices = batch_devices();
    select_devices(&mut state, devices.clone());
    state.handle_raw_events(vec![RawInputEvent::Text(TextCommit::new(
        "nothing matches",
    ))]);
    let refresh = refresh_tailnet(&mut state);
    let [ClientShellAction::MachineManager(Command::Tailscale { id, request })] =
        refresh.actions.as_slice()
    else {
        panic!()
    };
    let mut refreshed = devices;
    refreshed.reverse();
    refreshed[0].id = Some("replacement".into());
    state.receive_machine_event(Event::Tailscale {
        id: *id,
        request: *request,
        result: Ok(refreshed),
    });
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert_eq!(manager.tailscale.selected.len(), 3);
    state.machine_action(Hit::AddSelected, &mut ClientShellInput::default());
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    let batch = manager.batch.as_ref().unwrap();
    assert_eq!(batch.items[2].state.label(), "Skipped");
    assert!(batch.items[2].detail.contains("Not online"));
}
#[test]
fn batch_setup_runs_sequentially_continues_after_failure_and_retries_only_failed() {
    let mut state = shell();
    select_devices(&mut state, batch_devices());
    let mut start = ClientShellInput::default();
    state.machine_action(Hit::AddSelected, &mut start);
    let first = run_id(&start);
    assert!(state.take_next_machine_batch_command().is_none());
    assert!(state.receive_machine_event(Event::Finished {
        id: first,
        success: true,
        message: "Ready".into()
    }));
    let Some(Command::Run { id: second, .. }) = state.take_next_machine_batch_command() else {
        panic!()
    };
    assert_ne!(first, second);
    assert!(!state.receive_machine_event(Event::Finished {
        id: first,
        success: true,
        message: "Duplicate".into()
    }));
    assert!(state.receive_machine_event(Event::Finished {
        id: second,
        success: false,
        message: "Authentication failed".into()
    }));
    let Some(Command::Run { id: third, .. }) = state.take_next_machine_batch_command() else {
        panic!()
    };
    assert!(state.receive_machine_event(Event::Finished {
        id: third,
        success: true,
        message: "Ready".into()
    }));
    assert!(state.take_next_machine_batch_command().is_none());
    assert!(!state.receive_machine_event(Event::Finished {
        id: third,
        success: false,
        message: "Stale".into()
    }));
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert!(matches!(manager.page, Page::BatchResults));
    assert_eq!(
        manager
            .batch
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|i| i.state.label())
            .collect::<Vec<_>>(),
        ["Ready", "Failed", "Ready"]
    );
    let mut retry = ClientShellInput::default();
    state.machine_action(Hit::RetryBatch, &mut retry);
    assert!(
        matches!(retry.actions.as_slice(),[ClientShellAction::MachineManager(Command::Run{operation:Operation::AddChecked(form),..})] if form.target=="100.90.0.2")
    );
    let id = run_id(&retry);
    state.receive_machine_event(Event::Finished {
        id,
        success: true,
        message: "Ready".into(),
    });
    assert!(state.take_next_machine_batch_command().is_none());
}
#[test]
fn stopping_a_batch_during_approval_keeps_successes_and_never_launches_pending_servers() {
    let mut state = shell();
    select_devices(&mut state, batch_devices());
    let mut start = ClientShellInput::default();
    state.machine_action(Hit::AddSelected, &mut start);
    state.receive_machine_event(Event::Finished {
        id: run_id(&start),
        success: true,
        message: "Ready".into(),
    });
    let Some(Command::Run { id, .. }) = state.take_next_machine_batch_command() else {
        panic!()
    };
    state.receive_machine_event(Event::Approval {
        id,
        prompt: crate::remote::setup_progress::Approval {
            request: 1,
            title: "Install".into(),
            detail: "Install on Build 2?".into(),
            restart: false,
        },
    });
    let mut cancel = ClientShellInput::default();
    state.machine_action(Hit::Back, &mut cancel);
    assert!(cancel.actions.iter().any(|a|matches!(a,ClientShellAction::MachineManager(Command::Cancel{id:cancelled}) if *cancelled==id)));
    assert!(!state.receive_machine_event(Event::Finished {
        id,
        success: true,
        message: "Late".into()
    }));
    assert!(state.take_next_machine_batch_command().is_none());
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert_eq!(
        manager
            .batch
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|i| i.state.label())
            .collect::<Vec<_>>(),
        ["Ready", "Stopped", "Stopped"]
    );
    let mut close = ClientShellInput::default();
    state.machine_action(Hit::Close, &mut close);
    assert!(state.overlay.is_none());
    assert!(state.take_next_machine_batch_command().is_none());
}
#[test]
fn batch_username_is_frozen_and_changed_saved_profile_is_not_retargeted() {
    let mut state = shell();
    let saved = SavedSshEndpoint::new("Saved build", "alice@100.90.0.2", "main").unwrap();
    state.machine_profiles = vec![saved.clone()];
    select_devices(&mut state, batch_devices());
    let mut start = ClientShellInput::default();
    state.machine_action(Hit::AddSelected, &mut start);
    let first = run_id(&start);
    if let Some(ClientShellOverlay::Machines(manager)) = &mut state.overlay {
        manager.fields[5] = "bob".into();
    }
    state.machine_profiles[0].target = "alice@different-host".into();
    state.receive_machine_event(Event::Finished {
        id: first,
        success: true,
        message: "Ready".into(),
    });
    let Some(Command::Run {
        operation: Operation::AddChecked(form),
        ..
    }) = state.take_next_machine_batch_command()
    else {
        panic!()
    };
    assert_eq!(form.target, "100.90.0.3");
    assert_eq!(form.username, "alice");
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert_eq!(
        manager.batch.as_ref().unwrap().items[1].state.label(),
        "Failed"
    );
}

#[test]
fn batch_keyboard_space_selects_without_connecting_and_selection_is_bounded() {
    let mut state = shell();
    let (id, request) = quick_add(&mut state);
    let devices = (1..=33)
        .map(|n| {
            let mut d = tail_device(
                &format!("Build {n}"),
                Some(true),
                None,
                &format!("100.90.0.{n}"),
            );
            d.id = Some(format!("node-{n}"));
            d
        })
        .collect();
    state.receive_machine_event(Event::Tailscale {
        id,
        request,
        result: Ok(devices),
    });
    assert!(state.handle_input_bytes(b" ").actions.is_empty());
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert_eq!(manager.tailscale.selected.len(), 1);
    let revision = manager.tailscale.revision;
    for index in 1..33 {
        state.machine_action(
            Hit::ToggleDevice(index, revision),
            &mut ClientShellInput::default(),
        );
    }
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert_eq!(manager.tailscale.selected.len(), 32);
    assert!(manager.error.as_ref().unwrap().contains("32"));
}
#[test]
fn batch_results_do_not_interfere_with_a_later_single_server_setup() {
    let mut state = shell();
    let mut devices = batch_devices();
    devices.truncate(1);
    select_devices(&mut state, devices);
    let mut start = ClientShellInput::default();
    state.machine_action(Hit::AddSelected, &mut start);
    state.receive_machine_event(Event::Finished {
        id: run_id(&start),
        success: true,
        message: "Ready".into(),
    });
    let mut back = ClientShellInput::default();
    state.machine_action(Hit::Back, &mut back);
    if let Some(ClientShellOverlay::Machines(manager)) = &mut state.overlay {
        manager.page = Page::Manage(profile("Another"));
    }
    let mut setup = ClientShellInput::default();
    state.machine_action(Hit::Reconnect, &mut setup);
    let id = run_id(&setup);
    assert!(state.receive_machine_event(Event::Finished {
        id,
        success: true,
        message: "Ready".into()
    }));
    let Some(ClientShellOverlay::Machines(manager)) = &state.overlay else {
        panic!()
    };
    assert!(matches!(manager.page, Page::Done));
    assert!(manager.success);
}

#[test]
fn successful_batch_reconnect_immediately_retries_endpoint_but_failure_or_changed_identity_does_not(
) {
    for (success, changed) in [(true, false), (false, false), (true, true)] {
        let mut state = shell();
        let saved = SavedSshEndpoint::new("Saved build", "alice@100.90.0.1", "main").unwrap();
        state.set_endpoint_catalog(std::slice::from_ref(&saved));
        state.set_endpoint_status(
            &ClientEndpointId::Ssh(saved.id.clone()),
            ClientEndpointStatus::Attention,
        );
        let mut devices = batch_devices();
        devices.truncate(1);
        select_devices(&mut state, devices);
        let mut start = ClientShellInput::default();
        state.machine_action(Hit::AddSelected, &mut start);
        if changed {
            state.machine_profiles[0].target = "alice@another-host".into();
        }
        state.receive_machine_event(Event::Finished {
            id: run_id(&start),
            success,
            message: "Result".into(),
        });
        let retries = state.take_machine_retries();
        if success && !changed {
            assert_eq!(retries, vec![ClientEndpointId::Ssh(saved.id)]);
        } else {
            assert!(retries.is_empty());
        }
    }
}
#[test]
fn batch_checkboxes_summary_and_retained_failure_output_are_visible() {
    let mut state = shell();
    let mut devices = batch_devices();
    devices.truncate(1);
    select_devices(&mut state, devices);
    let text = state
        .compose(100, 36)
        .unwrap()
        .cells
        .iter()
        .map(|c| c.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("[x]"));
    assert!(text.contains("Add selected (1)"));
    let mut start = ClientShellInput::default();
    state.machine_action(Hit::AddSelected, &mut start);
    let id = run_id(&start);
    let mut screen = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 5));
    for (x, c) in "DENIED_BY_SSH".chars().enumerate() {
        screen[(x as u16, 0)].set_symbol(&c.to_string());
    }
    state.receive_machine_event(Event::Screen {
        id,
        buffer: screen,
        cursor: None,
    });
    state.receive_machine_event(Event::Finished {
        id,
        success: false,
        message: "Setup failed".into(),
    });
    let text = state
        .compose(100, 36)
        .unwrap()
        .cells
        .iter()
        .map(|c| c.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("Failed · Build 1"));
    assert!(text.contains("Retry failed / stopped"));
    state.machine_action(Hit::BatchItem(0), &mut ClientShellInput::default());
    let text = state
        .compose(100, 36)
        .unwrap()
        .cells
        .iter()
        .map(|c| c.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("DENIED_BY_SSH"));
    assert!(press(&mut state, KeyCode::Char('x')).actions.is_empty());
    press(&mut state, KeyCode::Esc);
    assert!(
        matches!(&state.overlay,Some(ClientShellOverlay::Machines(manager)) if matches!(manager.page,Page::BatchResults))
    );
}
