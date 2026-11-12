use super::super::super::tasks::{field, ClientTasksOverlay, TaskPage, TaskUiAction};
use super::*;
use ratatui::widgets::{Paragraph, Widget, Wrap};
use serde_json::Value;

fn text(value: &Value, name: &str, fallback: &str) -> String {
    let s = field(value, name);
    if s.is_empty() {
        fallback.to_owned()
    } else {
        s.to_owned()
    }
}

fn task_detail(task: &Value) -> String {
    let criteria = task["criteria"]
        .as_array()
        .map(|criteria| {
            criteria
                .iter()
                .filter_map(Value::as_str)
                .map(|item| format!("• {item}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let review = if task["review"].is_object() {
        format!(
            "\nReview: {}\n{}",
            field(&task["review"], "decision"),
            field(&task["review"], "note")
        )
    } else {
        String::new()
    };
    format!("{} · {}\n{}\n\nAgent: {}    Pane: {}\nBranch: {}\nDirectory: {}\n\nRequest\n{}\n\nAcceptance criteria\n{}\n\nResults\n{}\n\nTest evidence (reported by agent)\n{}{}",
        field(task,"id"),field(task,"status"),field(task,"title"),text(task,"agent","unassigned"),text(task,"pane_id","none"),text(task,"branch","not recorded"),text(task,"cwd","not recorded"),field(task,"brief"),if criteria.is_empty(){"Not recorded"}else{&criteria},text(task,"results","No result reported"),text(task,"tests","No test evidence reported"),review)
}

fn activity_text(activities: &[Value]) -> String {
    if activities.is_empty() {
        return "No matching activity.".into();
    }
    activities
        .iter()
        .map(|activity| {
            format!(
                "#{} · {} · {}\n{}",
                activity["seq"],
                field(activity, "kind"),
                text(activity, "task_id", "session"),
                field(activity, "text")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn summary_text(summary: Option<&Value>) -> String {
    let Some(summary) = summary else {
        return "Loading changes since your last acknowledgement…".into();
    };
    let tasks = |name: &str| {
        summary[name]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|task| format!("• {} · {}", field(task, "id"), field(task, "title")))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "None".into())
    };
    let failed = summary["failed_jobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .map(|job| {
                    format!(
                        "• {} · {} · {}",
                        field(job, "id"),
                        field(job, "task_id"),
                        field(job, "state")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "None".into());
    let activities = summary["activities"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    format!("Since your last acknowledgement (#{} → #{})\n\nReady for review\n{}\n\nBlocked\n{}\n\nFailed or interrupted jobs\n{}\n\nRecent changes\n{}\n\nAcknowledging marks only these changes as seen; it does not approve work.",summary["after"],summary["through"],tasks("needs_review"),tasks("blocked"),failed,activity_text(&activities))
}

pub(super) fn render_tasks_overlay(
    b: &mut Buffer,
    view: &ClientTasksOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let outer = popup(b.area, 104, 36)?;
    let inner = panel(b, outer, p.accent, p.panel_bg)?;
    let mut rendered = OverlayRender {
        area: outer,
        ..OverlayRender::default()
    };
    let base = Style::default().fg(p.text).bg(p.panel_bg);
    let muted = base.fg(p.subtext0);
    if inner.height < 10 || inner.width < 24 {
        put_text(
            b,
            inner.x,
            inner.y,
            inner.width,
            "Tasks · resize to view",
            base,
        );
        put_text(
            b,
            inner.x,
            inner.bottom().saturating_sub(1),
            inner.width,
            "esc close",
            muted,
        );
        return Some(rendered);
    }
    put_text(
        b,
        inner.x,
        inner.y,
        inner.width,
        "tasks & review",
        base.add_modifier(Modifier::BOLD),
    );
    let close = Rect::new(inner.right().saturating_sub(9), inner.y, 9, 1);
    put_text(b, close.x, close.y, close.width, "esc close", muted);
    rendered.task_actions.push((close, TaskUiAction::Close));
    let tab_width = inner.width / 4;
    for (i, (label, page)) in [
        ("1 inbox", TaskPage::Inbox),
        ("2 all tasks", TaskPage::All),
        ("3 activity", TaskPage::Activity),
        ("4 since last", TaskPage::Summary),
    ]
    .into_iter()
    .enumerate()
    {
        let rect = Rect::new(inner.x + tab_width * i as u16, inner.y + 2, tab_width, 1);
        let style = if view.page == page {
            base.fg(p.accent).add_modifier(Modifier::BOLD)
        } else {
            muted
        };
        put_text(b, rect.x, rect.y, rect.width, label, style);
        rendered.task_actions.push((rect, TaskUiAction::Page(page)));
    }
    let content = Rect::new(
        inner.x,
        inner.y + 4,
        inner.width,
        inner.height.saturating_sub(9),
    );
    let mut controls = vec![("r refresh", TaskUiAction::Refresh)];
    let mut show_input = false;
    match view.page {
        TaskPage::Inbox | TaskPage::All => {
            let tasks = view.visible_tasks();
            if tasks.is_empty() {
                put_text(
                    b,
                    content.x,
                    content.y,
                    content.width,
                    if view.loading > 0 {
                        "Loading tasks…"
                    } else if view.page == TaskPage::Inbox {
                        "Nothing needs review or input."
                    } else {
                        "No tasks yet. Create one with herdr task create."
                    },
                    muted,
                );
            }
            let offset = view
                .selected
                .saturating_sub((usize::from(content.height) / 2).saturating_sub(1));
            for (row, (index, task)) in tasks
                .iter()
                .enumerate()
                .skip(offset)
                .take(usize::from(content.height) / 2)
                .enumerate()
            {
                let rect = Rect::new(content.x, content.y + row as u16 * 2, content.width, 2);
                let style = if index == view.selected {
                    base.bg(p.surface0)
                } else {
                    base
                };
                b.set_style(rect, style);
                put_text(
                    b,
                    rect.x,
                    rect.y,
                    rect.width,
                    &format!(
                        "{} · {} · {}",
                        field(task, "id"),
                        field(task, "status"),
                        field(task, "title")
                    ),
                    style,
                );
                put_text(
                    b,
                    rect.x,
                    rect.y + 1,
                    rect.width,
                    &format!(
                        "{}  {}",
                        text(task, "agent", "unassigned"),
                        text(task, "branch", "no branch recorded")
                    ),
                    style.fg(p.subtext0),
                );
                rendered
                    .task_actions
                    .push((rect, TaskUiAction::Select(index)));
            }
            controls.push(("↵ open", TaskUiAction::Open));
        }
        page => {
            let body = match page {
                TaskPage::Detail => view
                    .selected_task()
                    .map(task_detail)
                    .unwrap_or_else(|| "Task no longer available. Refresh the task list.".into()),
                TaskPage::Activity => activity_text(&view.activities),
                TaskPage::Summary => summary_text(view.summary.as_ref()),
                TaskPage::Diff => format!(
                    "Working diff · git diff HEAD (staged and unstaged)\n\n{}",
                    if view.diff.is_empty() {
                        "No working changes."
                    } else {
                        &view.diff
                    }
                ),
                _ => String::new(),
            };
            let paragraph = Paragraph::new(body).style(base).wrap(Wrap { trim: false });
            let max_scroll = paragraph
                .line_count(content.width)
                .saturating_sub(usize::from(content.height));
            rendered.task_max_scroll = max_scroll;
            paragraph
                .scroll((
                    u16::try_from(view.scroll.min(max_scroll)).unwrap_or(u16::MAX),
                    0,
                ))
                .render(content, b);
            if max_scroll > 0 {
                let position = format!("{}/{}", view.scroll.min(max_scroll) + 1, max_scroll + 1);
                put_text(
                    b,
                    inner.right().saturating_sub(position.len() as u16),
                    inner.y,
                    position.len() as u16,
                    &position,
                    muted,
                );
            }
            match page {
                TaskPage::Detail => {
                    controls = vec![
                        ("a approve", TaskUiAction::Approve),
                        ("c changes", TaskUiAction::RequestChanges),
                        ("p pane", TaskUiAction::FocusPane),
                        ("d diff", TaskUiAction::Diff),
                        ("back", TaskUiAction::Back),
                    ];
                    show_input = true;
                }
                TaskPage::Activity => {
                    controls.push(("/ search", TaskUiAction::Search));
                    show_input = true;
                }
                TaskPage::Summary => controls.push(("a mark seen", TaskUiAction::Acknowledge)),
                TaskPage::Diff => controls.push(("back", TaskUiAction::Back)),
                _ => {}
            }
        }
    }
    if show_input {
        let input = Rect::new(inner.x, inner.bottom() - 4, inner.width, 1);
        let label = if view.page == TaskPage::Activity {
            "Search: "
        } else {
            "Note: "
        };
        put_text(b, input.x, input.y, label.len() as u16, label, muted);
        let editor = Rect::new(
            input.x + label.len() as u16,
            input.y,
            input.width.saturating_sub(label.len() as u16),
            1,
        );
        b.set_style(editor, base.bg(p.surface0));
        let cursor = text_editor::render(b, editor, &view.input, base.bg(p.surface0));
        if view.input_focused {
            rendered.cursor = cursor;
        }
        rendered.task_actions.push((input, TaskUiAction::Search));
        if view.input.as_str().is_empty() && !view.input_focused {
            put_text(
                b,
                editor.x,
                editor.y,
                editor.width,
                if view.page == TaskPage::Activity {
                    "click or /, type, then Enter"
                } else {
                    "click or / to enter a review / follow-up note"
                },
                muted,
            );
        }
    }
    if let Some(error) = view.error.as_deref() {
        put_text(
            b,
            inner.x,
            inner.bottom() - 3,
            inner.width,
            error,
            base.fg(p.red),
        );
    } else if view.loading > 0 {
        put_text(
            b,
            inner.x,
            inner.bottom() - 3,
            inner.width,
            "Waiting for server…",
            base.fg(p.accent),
        );
    }
    let mut x = inner.x;
    let mut y = inner.bottom() - 2;
    for (label, action) in controls {
        let width = (label.len() as u16).saturating_add(2).min(inner.width);
        if x + width > inner.right() {
            x = inner.x;
            y = y.saturating_add(1);
        }
        if y >= inner.bottom() {
            break;
        }
        let rect = Rect::new(x, y, width, 1);
        put_text(b, x, y, width, label, base.fg(p.accent).bg(p.surface0));
        rendered.task_actions.push((rect, action));
        x += width + 1;
    }
    Some(rendered)
}
