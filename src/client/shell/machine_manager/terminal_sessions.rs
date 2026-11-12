use super::*;

pub(super) fn rows(manager: &Manager) -> Vec<(String, Hit)> {
    let mut rows = vec![(
        if manager.import_loading {
            if matches!(
                manager.last_operation,
                Some(Operation::ImportSession { .. })
            ) {
                "Import starting; closing this dialog leaves it running."
            } else {
                "Refreshing running terminals…"
            }
        } else {
            "Refresh running terminals"
        }
        .into(),
        if manager.import_loading {
            Hit::Hint
        } else {
            Hit::RefreshImports
        },
    )];
    let Some(report) = &manager.import_report else {
        return rows;
    };
    if !report.supported {
        rows.push((report.message.clone(), Hit::Hint));
        return rows;
    }
    if !report.helper_ready {
        rows.push((
            "Enable import helper (administrator access)…".into(),
            Hit::EnableImports,
        ));
        rows.push((
            "Install reptyr and a protected helper on this server.".into(),
            Hit::Hint,
        ));
    } else {
        rows.push((
            "Move a whole terminal; existing programs keep running.".into(),
            Hit::Hint,
        ));
    }
    if report.sessions.is_empty() {
        rows.push((
            "No external terminals found for this account.".into(),
            Hit::Hint,
        ));
    }
    for (index, session) in report.sessions.iter().enumerate() {
        rows.push((
            format!(
                "{} · {} · {} processes · {}",
                session.terminal, session.command, session.process_count, session.cwd
            ),
            if report.helper_ready && !manager.import_loading {
                Hit::ImportTerminal(index)
            } else {
                Hit::Hint
            },
        ));
    }
    rows
}

pub(super) fn scan(manager: &mut Manager, outcome: &mut ClientShellInput) {
    let Page::Imports(profile) = &manager.page else {
        return;
    };
    manager.import_request = machine_setup::next_id();
    manager.last_operation = None;
    manager.import_loading = true;
    manager.error = None;
    outcome
        .actions
        .push(ClientShellAction::MachineManager(Command::TerminalScan {
            id: manager.id,
            request: manager.import_request,
            profile: profile.clone(),
        }));
}
