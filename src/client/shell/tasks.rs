//! Client-owned task browser. All task facts arrive through the selected server's JSON API.
use super::*;
use serde_json::{json, Value};

const SUMMARY_CONSUMER: &str = "tui-review";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskPage {
    Inbox,
    All,
    Activity,
    Summary,
    Detail,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskUiAction {
    Page(TaskPage),
    Select(usize),
    Open,
    Refresh,
    Search,
    Approve,
    RequestChanges,
    FocusPane,
    Diff,
    Acknowledge,
    Close,
    Back,
}

#[derive(Debug, Clone)]
pub(super) enum TaskRequestKind {
    List,
    Activity { query: String },
    Summary,
    Review { task_id: String },
    Diff { task_id: String },
    Acknowledge,
}

#[derive(Debug)]
pub(super) struct ClientTasksOverlay {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) overlay_id: u64,
    pub(super) page: TaskPage,
    pub(super) tasks: Vec<Value>,
    pub(super) activities: Vec<Value>,
    pub(super) summary: Option<Value>,
    pub(super) selected: usize,
    pub(super) task_id: Option<String>,
    pub(super) input: TextEditor,
    pub(super) input_focused: bool,
    pub(super) scroll: usize,
    pub(super) loading: usize,
    pub(super) error: Option<String>,
    pub(super) diff: String,
}

impl ClientTasksOverlay {
    pub(super) fn visible_tasks(&self) -> Vec<&Value> {
        self.tasks
            .iter()
            .filter(|task| {
                if self.page != TaskPage::Inbox {
                    return true;
                }
                matches!(
                    task["status"].as_str(),
                    Some(
                        "ready_for_review"
                            | "review"
                            | "blocked"
                            | "needs_review"
                            | "failed"
                            | "interrupted"
                    )
                )
            })
            .collect()
    }
    pub(super) fn selected_task(&self) -> Option<&Value> {
        if matches!(self.page, TaskPage::Detail | TaskPage::Diff) {
            self.tasks
                .iter()
                .find(|task| task["id"].as_str() == self.task_id.as_deref())
        } else {
            self.visible_tasks().get(self.selected).copied()
        }
    }
    fn move_selection(&mut self, delta: isize) {
        if matches!(self.page, TaskPage::Inbox | TaskPage::All) {
            self.selected = self
                .selected
                .saturating_add_signed(delta)
                .min(self.visible_tasks().len().saturating_sub(1));
        } else {
            self.scroll = self.scroll.saturating_add_signed(delta);
        }
    }
}

pub(super) fn field<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or("")
}

impl ClientShellState {
    pub(super) fn open_tasks_overlay(&mut self, outcome: &mut ClientShellInput) {
        // A feature must be explicitly advertised. Legacy optimistic fallback is unsuitable
        // for a newly introduced runtime feature.
        let advertised = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(|endpoint| endpoint.methods.as_ref())
            .is_some_and(|methods| methods.iter().any(|method| method == "task.list"));
        if !advertised {
            self.push_endpoint_notice(
                ClientEndpointNoticeKind::Unsupported,
                "task.list",
                "Tasks unavailable",
                "Update and restart this server to enable tasks & review.",
            );
            outcome.repaint = true;
            return;
        }
        self.overlay = Some(ClientShellOverlay::Tasks(ClientTasksOverlay {
            endpoint_id: self.active_endpoint_id.clone(),
            overlay_id: self.next_request_id,
            page: TaskPage::Inbox,
            tasks: Vec::new(),
            activities: Vec::new(),
            summary: None,
            selected: 0,
            task_id: None,
            input: TextEditor::default(),
            input_focused: false,
            scroll: 0,
            loading: 0,
            error: None,
            diff: String::new(),
        }));
        self.refresh_tasks(outcome);
    }

    fn request_task(
        &mut self,
        method: &str,
        params: Value,
        request: TaskRequestKind,
        outcome: &mut ClientShellInput,
    ) {
        let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_ref() else {
            return;
        };
        if view.endpoint_id != self.active_endpoint_id {
            return;
        }
        let endpoint_id = view.endpoint_id.clone();
        let overlay_id = view.overlay_id;
        let method = match serde_json::from_value(json!({"method":method,"params":params})) {
            Ok(method) => method,
            Err(error) => {
                if let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() {
                    view.error = Some(format!("Cannot prepare task request: {error}"));
                }
                outcome.repaint = true;
                return;
            }
        };
        let accepted = self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::Tasks {
                endpoint_id,
                overlay_id,
                request,
            },
            outcome,
        );
        if let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() {
            if accepted {
                view.loading += 1;
                view.error = None;
            } else {
                view.error = Some("This task action is unavailable on the selected server.".into());
            }
        }
        outcome.repaint = true;
    }

    fn refresh_tasks(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_ref() else {
            return;
        };
        match view.page {
            TaskPage::Summary => self.request_task(
                "task.summary",
                json!({"consumer":SUMMARY_CONSUMER}),
                TaskRequestKind::Summary,
                outcome,
            ),
            TaskPage::Activity => {
                let query = view.input.to_string();
                self.request_task(
                    "task.activity",
                    json!({"query":query,"after":0,"limit":200}),
                    TaskRequestKind::Activity { query },
                    outcome,
                );
            }
            TaskPage::Diff => {
                if let Some(task_id) = view.task_id.clone() {
                    self.request_task(
                        "task.diff",
                        json!({"task_id":task_id}),
                        TaskRequestKind::Diff { task_id },
                        outcome,
                    );
                }
            }
            _ => self.request_task("task.list", json!({}), TaskRequestKind::List, outcome),
        }
    }

    pub(super) fn task_ui_action(&mut self, action: TaskUiAction, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_ref() else {
            return;
        };
        if view.endpoint_id != self.active_endpoint_id {
            self.overlay = None;
            outcome.repaint = true;
            return;
        }
        match action {
            TaskUiAction::Close => {
                self.overlay = None;
            }
            TaskUiAction::Page(page) => {
                if let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() {
                    view.page = page;
                    view.selected = 0;
                    view.scroll = 0;
                    view.input.clear();
                    view.input_focused = false;
                    view.error = None;
                }
                self.refresh_tasks(outcome);
            }
            TaskUiAction::Select(index) => {
                if let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() {
                    view.selected = index;
                }
                self.task_ui_action(TaskUiAction::Open, outcome);
            }
            TaskUiAction::Open => {
                let task_id = view
                    .selected_task()
                    .and_then(|task| task["id"].as_str())
                    .map(str::to_owned);
                if let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() {
                    if let Some(task_id) = task_id {
                        view.task_id = Some(task_id);
                        view.page = TaskPage::Detail;
                        view.scroll = 0;
                        view.input.clear();
                        view.input_focused = false;
                    }
                }
            }
            TaskUiAction::Refresh => self.refresh_tasks(outcome),
            TaskUiAction::Search => {
                if let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() {
                    view.input_focused = true;
                }
            }
            TaskUiAction::Approve | TaskUiAction::RequestChanges => {
                if view.loading > 0 {
                    return;
                }
                let task_id = view
                    .selected_task()
                    .and_then(|task| task["id"].as_str())
                    .map(str::to_owned);
                let note = view.input.to_string();
                if action == TaskUiAction::RequestChanges && note.trim().is_empty() {
                    if let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() {
                        view.input_focused = true;
                        view.error = Some("Enter a follow-up note, then request changes.".into());
                    }
                } else if let Some(task_id) = task_id {
                    let decision = if action == TaskUiAction::Approve {
                        "accept"
                    } else {
                        "request_changes"
                    };
                    self.request_task(
                        "task.review",
                        json!({"task_id":task_id,"decision":decision,"note":note}),
                        TaskRequestKind::Review { task_id },
                        outcome,
                    );
                }
            }
            TaskUiAction::FocusPane => {
                let pane_id = view
                    .selected_task()
                    .and_then(|task| task["pane_id"].as_str())
                    .map(str::to_owned);
                if let Some(pane_id) = pane_id {
                    if self.snapshot.as_deref().is_some_and(|snapshot| {
                        snapshot.panes.iter().any(|pane| pane.pane_id == pane_id)
                    }) {
                        self.overlay = None;
                        self.push_endpoint_method(
                            crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                                pane_id,
                            }),
                            outcome,
                        );
                    } else if let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() {
                        view.error = Some(
                            "The original pane is no longer available; task results are retained."
                                .into(),
                        );
                    }
                }
            }
            TaskUiAction::Diff => {
                if let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() {
                    view.page = TaskPage::Diff;
                    view.scroll = 0;
                    view.diff.clear();
                    view.input_focused = false;
                }
                self.refresh_tasks(outcome);
            }
            TaskUiAction::Acknowledge => {
                if view.loading > 0 {
                    return;
                }
                if let Some(through) = view
                    .summary
                    .as_ref()
                    .and_then(|summary| summary["through"].as_u64())
                {
                    self.request_task(
                        "task.acknowledge",
                        json!({"consumer":SUMMARY_CONSUMER,"through":through}),
                        TaskRequestKind::Acknowledge,
                        outcome,
                    );
                }
            }
            TaskUiAction::Back => {
                if let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() {
                    view.page = if view.page == TaskPage::Diff {
                        TaskPage::Detail
                    } else {
                        TaskPage::Inbox
                    };
                    view.scroll = 0;
                    view.input_focused = false;
                    view.error = None;
                }
            }
        }
        outcome.repaint = true;
    }

    pub(super) fn insert_task_overlay_text(&mut self, text: &str) -> bool {
        match self.overlay.as_mut() {
            Some(ClientShellOverlay::Tasks(view)) if view.input_focused => {
                view.input.insert(text);
                true
            }
            _ => false,
        }
    }

    pub(super) fn route_task_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() else {
            return false;
        };
        if view.input_focused {
            if key.code == KeyCode::Esc {
                view.input_focused = false;
            } else if key.code == KeyCode::Enter {
                view.input_focused = false;
                if view.page == TaskPage::Activity {
                    self.refresh_tasks(outcome);
                }
            } else {
                view.input.handle_key(key);
            }
            outcome.repaint = true;
            return true;
        }
        let action = match key.code {
            KeyCode::Esc if matches!(view.page, TaskPage::Detail | TaskPage::Diff) => {
                Some(TaskUiAction::Back)
            }
            KeyCode::Esc | KeyCode::Char('q') => Some(TaskUiAction::Close),
            KeyCode::Char('1') => Some(TaskUiAction::Page(TaskPage::Inbox)),
            KeyCode::Char('2') => Some(TaskUiAction::Page(TaskPage::All)),
            KeyCode::Char('3') => Some(TaskUiAction::Page(TaskPage::Activity)),
            KeyCode::Char('4') => Some(TaskUiAction::Page(TaskPage::Summary)),
            KeyCode::Char('/') => Some(TaskUiAction::Search),
            KeyCode::Char('r') => Some(TaskUiAction::Refresh),
            KeyCode::Enter if matches!(view.page, TaskPage::Inbox | TaskPage::All) => {
                Some(TaskUiAction::Open)
            }
            KeyCode::Char('a') if view.page == TaskPage::Detail => Some(TaskUiAction::Approve),
            KeyCode::Char('c') if view.page == TaskPage::Detail => {
                Some(TaskUiAction::RequestChanges)
            }
            KeyCode::Char('p') if view.page == TaskPage::Detail => Some(TaskUiAction::FocusPane),
            KeyCode::Char('d') if view.page == TaskPage::Detail => Some(TaskUiAction::Diff),
            KeyCode::Char('a') if view.page == TaskPage::Summary => Some(TaskUiAction::Acknowledge),
            KeyCode::Up | KeyCode::Char('k') => {
                view.move_selection(-1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                view.move_selection(1);
                None
            }
            KeyCode::PageUp => {
                view.move_selection(-10);
                None
            }
            KeyCode::PageDown => {
                view.move_selection(10);
                None
            }
            KeyCode::Home => {
                view.selected = 0;
                view.scroll = 0;
                None
            }
            _ => None,
        };
        if let Some(action) = action {
            self.task_ui_action(action, outcome);
        }
        outcome.repaint = true;
        true
    }

    pub(super) fn handle_task_overlay_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() else {
            return false;
        };
        match mouse.kind {
            MouseEventKind::ScrollUp => view.move_selection(-3),
            MouseEventKind::ScrollDown => view.move_selection(3),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(action) = self.hits.task_actions.iter().find_map(|(rect, action)| {
                    super::contains(*rect, (mouse.column, mouse.row)).then_some(*action)
                }) {
                    self.task_ui_action(action, outcome);
                }
            }
            _ => {}
        }
        outcome.repaint = true;
        true
    }

    pub(super) fn complete_task_request(
        &mut self,
        endpoint_id: ClientEndpointId,
        overlay_id: u64,
        request: TaskRequestKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        if self.active_endpoint_id != endpoint_id {
            return (false, Vec::new());
        }
        let Some(ClientShellOverlay::Tasks(view)) = self.overlay.as_mut() else {
            return (false, Vec::new());
        };
        if view.overlay_id != overlay_id || view.endpoint_id != endpoint_id {
            return (false, Vec::new());
        }
        view.loading = view.loading.saturating_sub(1);
        let value = match result.and_then(|result| {
            serde_json::to_value(result).map_err(|error| ClientShellEndpointError {
                code: None,
                message: error.to_string(),
            })
        }) {
            Ok(value) => value,
            Err(error) => {
                view.error = Some(error.message);
                return (true, Vec::new());
            }
        };
        let expected = match &request {
            TaskRequestKind::List => "task_list",
            TaskRequestKind::Activity { .. } => "task_activity",
            TaskRequestKind::Summary => "task_summary",
            TaskRequestKind::Review { .. } => "task_info",
            TaskRequestKind::Diff { .. } => "task_diff",
            TaskRequestKind::Acknowledge => "ok",
        };
        if value["type"] != expected {
            view.error = Some("Server returned an unexpected task result.".into());
            return (true, Vec::new());
        }
        let mut outcome = ClientShellInput::default();
        match request {
            TaskRequestKind::List => {
                let selected_id = view
                    .selected_task()
                    .map(|task| field(task, "id").to_owned());
                view.tasks = value["tasks"].as_array().cloned().unwrap_or_default();
                view.tasks.reverse();
                if let Some(selected_id) = selected_id {
                    view.selected = view
                        .visible_tasks()
                        .iter()
                        .position(|task| field(task, "id") == selected_id)
                        .unwrap_or(0);
                }
                view.selected = view
                    .selected
                    .min(view.visible_tasks().len().saturating_sub(1));
            }
            TaskRequestKind::Activity { query } if query == view.input.as_str() => {
                view.activities = value["activities"].as_array().cloned().unwrap_or_default();
                view.scroll = 0;
            }
            TaskRequestKind::Activity { .. } => {}
            TaskRequestKind::Summary => {
                view.summary = Some(value["summary"].clone());
                view.scroll = 0;
            }
            TaskRequestKind::Review { task_id } => {
                if let Some(task) = view
                    .tasks
                    .iter_mut()
                    .find(|task| field(task, "id") == task_id)
                {
                    *task = value["task"].clone();
                }
                if view.task_id.as_deref() == Some(task_id.as_str()) {
                    view.input.clear();
                    view.error = None;
                }
            }
            TaskRequestKind::Diff { task_id }
                if view.task_id.as_deref() == Some(task_id.as_str()) =>
            {
                view.diff = field(&value, "diff").to_owned();
            }
            TaskRequestKind::Diff { .. } => {}
            TaskRequestKind::Acknowledge => self.refresh_tasks(&mut outcome),
        }
        (true, outcome.actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ClientShellState {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        state.set_snapshot(Box::new(super::super::tests::snapshot()));
        state.set_endpoint_methods(Some(
            [
                "task.list",
                "task.review",
                "task.summary",
                "task.activity",
                "task.acknowledge",
                "task.diff",
                "pane.focus",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ));
        state
    }

    fn response(value: Value) -> crate::api::schema::ResponseResult {
        serde_json::from_value(value).unwrap()
    }
    fn task(id: &str, status: &str) -> Value {
        json!({"id":id,"title":format!("Task {id}"),"brief":"Fix the login redirect","criteria":["Tests pass"],"agent":"codex","pane_id":"pane_1","branch":"fix/login","cwd":"/repo","status":status,"results":"Changed redirect handling","tests":"just test passed","review":null,"job_ids":[],"workflow_run_id":null,"workflow_step_id":null,"created_at_ms":1,"updated_at_ms":2})
    }
    fn request_id(outcome: &ClientShellInput) -> String {
        match &outcome.actions[0] {
            ClientShellAction::Endpoint { request, .. } => request.id.clone(),
            _ => panic!("expected endpoint request"),
        }
    }
    pub(super) fn opened() -> ClientShellState {
        let mut state = state();
        let mut outcome = ClientShellInput::default();
        state.open_tasks_overlay(&mut outcome);
        state.handle_endpoint_result(
            "boot-1",
            &request_id(&outcome),
            Ok(response(
                json!({"type":"task_list","tasks":[task("t1","review"),task("t2","running")]}),
            )),
        );
        state
    }

    #[test]
    fn tasks_require_advertised_methods_and_leave_other_actions_available() {
        let mut state = state();
        state.set_endpoint_methods(Some(vec!["pane.focus".into()]));
        let mut outcome = ClientShellInput::default();
        state.open_tasks_overlay(&mut outcome);
        assert!(state.overlay.is_none());
        assert!(outcome.actions.is_empty());
        assert!(
            state.supports_endpoint_method(&crate::api::schema::Method::PaneFocus(
                crate::api::schema::PaneTarget {
                    pane_id: "pane_1".into()
                }
            ))
        );
        assert!(state.visible_endpoint_notice.is_some());
    }

    #[test]
    fn task_inbox_filters_running_work_and_review_keeps_evidence() {
        let mut state = opened();
        let Some(ClientShellOverlay::Tasks(view)) = state.overlay.as_ref() else {
            panic!()
        };
        assert_eq!(view.visible_tasks().len(), 1);
        state.handle_input_bytes(b"\r");
        let Some(ClientShellOverlay::Tasks(view)) = state.overlay.as_ref() else {
            panic!()
        };
        assert_eq!(view.task_id.as_deref(), Some("t1"));
        assert_eq!(view.page, TaskPage::Detail);
        assert_eq!(
            field(view.selected_task().unwrap(), "tests"),
            "just test passed"
        );
        let outcome = state.handle_input_bytes(b"c");
        assert!(
            outcome.actions.is_empty(),
            "request changes needs a deliberate note"
        );
        state.handle_input_bytes(b"Add a regression test\r");
        let outcome = state.handle_input_bytes(b"c");
        let ClientShellAction::Endpoint { request, .. } = &outcome.actions[0] else {
            panic!()
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["method"], "task.review");
        assert_eq!(value["params"]["decision"], "request_changes");
        assert_eq!(value["params"]["note"], "Add a regression test");
    }

    #[test]
    fn task_activity_search_submits_only_on_enter_and_discards_stale_queries() {
        let mut state = opened();
        state.handle_input_bytes(b"3");
        state.handle_input_bytes(b"/login");
        let outcome = state.handle_input_bytes(b"\r");
        let id = request_id(&outcome);
        let ClientShellAction::Endpoint { request, .. } = &outcome.actions[0] else {
            panic!()
        };
        assert_eq!(
            serde_json::to_value(request).unwrap()["params"]["query"],
            "login"
        );
        state.handle_input_bytes(b"/other");
        state.handle_endpoint_result("boot-1",&id,Ok(response(json!({"type":"task_activity","activities":[{"seq":1,"at_ms":1,"kind":"task.created","task_id":"t1","job_id":null,"text":"stale result"}]}))));
        let Some(ClientShellOverlay::Tasks(view)) = state.overlay.as_ref() else {
            panic!()
        };
        assert!(view.activities.is_empty());
    }

    #[test]
    fn task_summary_is_acknowledged_only_by_explicit_action() {
        let mut state = opened();
        let outcome = state.handle_input_bytes(b"4");
        state.handle_endpoint_result("boot-1",&request_id(&outcome),Ok(response(json!({"type":"task_summary","summary":{"consumer":SUMMARY_CONSUMER,"after":0,"through":42,"activities":[],"needs_review":[],"blocked":[],"failed_jobs":[]}}))));
        assert!(state.handle_input_bytes(b"\x1b").actions.is_empty());
        state = opened();
        let outcome = state.handle_input_bytes(b"4");
        state.handle_endpoint_result("boot-1",&request_id(&outcome),Ok(response(json!({"type":"task_summary","summary":{"consumer":SUMMARY_CONSUMER,"after":0,"through":42,"activities":[],"needs_review":[],"blocked":[],"failed_jobs":[]}}))));
        let outcome = state.handle_input_bytes(b"a");
        let ClientShellAction::Endpoint { request, .. } = &outcome.actions[0] else {
            panic!()
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["method"], "task.acknowledge");
        assert_eq!(value["params"]["through"], 42);
    }

    #[test]
    fn replies_from_closed_task_browser_cannot_populate_reopened_browser() {
        let mut state = state();
        let mut outcome = ClientShellInput::default();
        state.open_tasks_overlay(&mut outcome);
        let old_id = request_id(&outcome);
        state.handle_input_bytes(b"\x1b");
        let mut outcome = ClientShellInput::default();
        state.open_tasks_overlay(&mut outcome);
        state.handle_endpoint_result(
            "boot-1",
            &old_id,
            Ok(response(
                json!({"type":"task_list","tasks":[task("old","review")]}),
            )),
        );
        let Some(ClientShellOverlay::Tasks(view)) = state.overlay.as_ref() else {
            panic!()
        };
        assert!(view.tasks.is_empty());
        assert_eq!(view.loading, 1);
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    #[test]
    fn task_overlay_renders_and_mouse_opens_task_at_desktop_and_narrow_sizes() {
        let mut state = super::tests::opened();
        for (cols, rows) in [(106, 40), (55, 20), (28, 14), (16, 8)] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
            let rendered = render::render_client_overlay(
                &mut buffer,
                state.overlay.as_ref().unwrap(),
                state.snapshot.as_deref().unwrap(),
                &state.endpoints,
                &state.active_endpoint_id,
                &state.config.keybinds,
                &state.config.palette,
            )
            .unwrap();
            assert!(buffer
                .area
                .contains((rendered.area.x, rendered.area.y).into()));
            assert!(rendered.area.right() <= cols);
            assert!(rendered.area.bottom() <= rows);
            if cols == 106 {
                let row = rendered
                    .task_actions
                    .iter()
                    .find_map(|(rect, action)| {
                        matches!(action, TaskUiAction::Select(_)).then_some(*rect)
                    })
                    .unwrap();
                state.hits.task_actions = rendered.task_actions;
                let mut outcome = ClientShellInput::default();
                state.handle_mouse(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: row.x,
                        row: row.y,
                        modifiers: KeyModifiers::empty(),
                    },
                    &mut outcome,
                );
                assert!(matches!(
                    state.overlay,
                    Some(ClientShellOverlay::Tasks(ClientTasksOverlay {
                        page: TaskPage::Detail,
                        ..
                    }))
                ));
                assert!(
                    outcome.requests.is_empty(),
                    "mouse selection must not type into the agent"
                );
            }
        }
    }
}
