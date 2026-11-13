//! Server-owned tasks. Filesystem work happens on commands/lifecycle events, never render.
use super::responses::{encode_error, encode_error_body, encode_success};
use crate::api::schema::*;
use crate::app::App;
use crate::tasks::TaskStore;

pub(super) fn is_task_method(method: &Method) -> bool {
    let name = method.operation().name;
    name.starts_with("task.") || name.starts_with("job.") || name.starts_with("workflow.")
}

impl App {
    fn tasks(&mut self) -> Result<&mut TaskStore, String> {
        if self.task_store.is_none() {
            self.task_store = Some(if self.policy.persist_session {
                TaskStore::open(crate::session::data_dir().join("tasks.json"))?
            } else {
                TaskStore::memory()
            });
        }
        self.task_store
            .as_mut()
            .ok_or_else(|| "task store unavailable".into())
    }

    fn poll_task_submissions(&mut self) {
        let finished: Vec<_> = self
            .task_submissions
            .iter()
            .filter_map(|(id, receiver)| match receiver.try_recv() {
                Ok(Ok(())) => Some((id.clone(), None)),
                Ok(Err(error)) => Some((id.clone(), Some(error.to_string()))),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some((
                    id.clone(),
                    Some(
                        "prompt delivery channel closed; inspect the agent before retrying".into(),
                    ),
                )),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            })
            .collect();
        for (id, error) in finished {
            self.task_submissions.remove(&id);
            if let Some(error) = error {
                if let Ok(store) = self.tasks() {
                    if let Err(error) = store.interrupt_job(&id, &error) {
                        tracing::warn!(%error, job_id = %id, "could not persist prompt delivery failure");
                    }
                }
            }
        }
    }

    pub(super) fn handle_task_request(&mut self, request: Request) -> String {
        self.poll_task_submissions();
        let id = request.id;
        // Check storage before touching an agent or returning an empty list.
        if let Err(error) = self.tasks() {
            return encode_error(id, "task_store_unavailable", error);
        }
        if let Err(error) = self.reconcile_task_locations() {
            return encode_error(id, "task_store_unavailable", error);
        }
        let result = self.task_operation(request.method);
        match result {
            Ok(result) => encode_success(id, result),
            Err(error) => encode_error_body(id, error),
        }
    }

    fn reconcile_task_locations(&mut self) -> Result<(), String> {
        let bindings: Vec<_> = self
            .task_store
            .as_ref()
            .into_iter()
            .flat_map(|store| {
                store
                    .data()
                    .jobs
                    .values()
                    .filter_map(|job| job.terminal_id.clone())
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        // Resolve only recorded terminals, on a task command. This never runs in render.
        let mut locations = Vec::new();
        for terminal_id in bindings {
            for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
                for tab in &ws.tabs {
                    if let Some((&pane_id, _)) = tab
                        .panes
                        .iter()
                        .find(|(_, pane)| pane.attached_terminal_id.as_str() == terminal_id)
                    {
                        if let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) {
                            locations.push((terminal_id.clone(), pane_id));
                        }
                    }
                }
            }
        }
        let store = self.tasks()?;
        for (terminal_id, pane_id) in locations {
            store.relocate_terminal(&terminal_id, &pane_id)?;
        }
        Ok(())
    }

    fn task_operation(&mut self, method: Method) -> Result<ResponseResult, ErrorBody> {
        match method {
            Method::JobStart(params) => self.start_tracked_job(params),
            Method::TaskDiff(_) => Err(task_error(
                "working diff requires the background server".into(),
            )),
            method => {
                let store = self.tasks().map_err(task_error)?;
                let result: Result<ResponseResult, String> = (|| {
                    Ok(match method {
                        Method::TaskCreate(params) => ResponseResult::TaskInfo {
                            task: store.create_task(params)?,
                        },
                        Method::TaskUpdate(params) => ResponseResult::TaskInfo {
                            task: store.update_task(&params.task_id, &params.patch)?,
                        },
                        Method::TaskList(_) => ResponseResult::TaskList {
                            tasks: store.data().tasks.values().cloned().collect(),
                        },
                        Method::TaskGet(params) => ResponseResult::TaskInfo {
                            task: store
                                .data()
                                .tasks
                                .get(&params.task_id)
                                .cloned()
                                .ok_or("task not found")?,
                        },
                        Method::TaskReview(params) => ResponseResult::TaskInfo {
                            task: store.review_task(
                                &params.task_id,
                                params.decision,
                                &params.note,
                            )?,
                        },
                        Method::TaskActivity(params) => ResponseResult::TaskActivity {
                            activities: store.activity(
                                params.query.as_deref(),
                                params.after,
                                params.limit,
                            ),
                        },
                        Method::TaskSummary(params) => ResponseResult::TaskSummary {
                            summary: store.summary(&params.consumer),
                        },
                        Method::TaskAcknowledge(params) => {
                            store.acknowledge(&params.consumer, params.through)?;
                            ResponseResult::Ok {}
                        }
                        Method::JobGet(params) => ResponseResult::JobInfo {
                            job: store
                                .data()
                                .jobs
                                .get(&params.job_id)
                                .cloned()
                                .ok_or("job not found")?,
                        },
                        Method::JobReport(params) => ResponseResult::JobInfo {
                            job: store.report_job(&params.job_id, &params.token, params.report)?,
                        },
                        Method::JobCancel(params) => ResponseResult::JobInfo {
                            job: store.cancel_job(&params.job_id)?,
                        },
                        Method::WorkflowSave(params) => ResponseResult::WorkflowTemplateInfo {
                            template: store.save_template(params)?,
                        },
                        Method::WorkflowList(_) => ResponseResult::WorkflowList {
                            templates: store.data().templates.values().cloned().collect(),
                            runs: store.data().workflows.values().cloned().collect(),
                        },
                        Method::WorkflowStart(params) => {
                            let run = store.start_workflow(
                                &params.template_id,
                                &params.brief,
                                params.cwd,
                            )?;
                            let ready_steps = store.ready_steps(&run.id)?;
                            ResponseResult::WorkflowInfo { run, ready_steps }
                        }
                        Method::WorkflowGet(params) => ResponseResult::WorkflowInfo {
                            run: store
                                .workflow_run(&params.run_id)
                                .cloned()
                                .ok_or("workflow run not found")?,
                            ready_steps: store.ready_steps(&params.run_id)?,
                        },
                        Method::WorkflowStepStart(params) => ResponseResult::TaskInfo {
                            task: store.start_step(&params.run_id, &params.step_id)?,
                        },
                        _ => return Err("unknown task operation".into()),
                    })
                })();
                result.map_err(task_error)
            }
        }
    }

    fn start_tracked_job(&mut self, params: JobStartParams) -> Result<ResponseResult, ErrorBody> {
        // Deduplicate before checking readiness. A retry must never submit text twice,
        // including after a server restart or if the original response was lost.
        if let Some(job) = self
            .tasks()
            .map_err(task_error)?
            .data()
            .jobs
            .values()
            .find(|job| job.request_key == params.request_key)
            .cloned()
        {
            let same_target = job.launch_target.as_deref().map_or_else(
                || params.target == job.agent || params.target == job.pane_id,
                |target| target == params.target,
            );
            if job.task_id != params.task_id || !same_target {
                return Err(task_error(
                    "request key already belongs to different job parameters".into(),
                ));
            }
            return Ok(ResponseResult::JobInfo { job });
        }
        self.reconcile_managed_agent_target(&params.target);
        let agent = self.tracked_agent_ready(&params.target)?;
        let store = self.tasks().map_err(task_error)?;
        if store
            .data()
            .jobs
            .values()
            .any(|job| job.state.active() && job.terminal_id.as_deref() == Some(&agent.terminal_id))
        {
            return Err(task_error("terminal already has an active job".into()));
        }
        let existing = store
            .data()
            .tasks
            .get(&params.task_id)
            .cloned()
            .ok_or_else(|| task_error("task not found".into()))?;
        if existing.cwd.is_none() {
            if let Some(cwd) = agent.foreground_cwd.clone().or_else(|| agent.cwd.clone()) {
                store
                    .update_task(
                        &params.task_id,
                        &TaskPatch {
                            cwd: Some(cwd),
                            ..Default::default()
                        },
                    )
                    .map_err(task_error)?;
            }
        }
        let previous_context = existing
            .review
            .as_ref()
            .map(|review| {
                format!(
                    "\n\nPrevious result:\n{}\nTest evidence:\n{}\nReviewer follow-up:\n{}",
                    existing.results, existing.tests, review.note
                )
            })
            .unwrap_or_default();
        let job = self
            .tasks()
            .map_err(task_error)?
            .start_job(
                &params.task_id,
                &params.request_key,
                &agent.pane_id,
                agent.name.as_deref().unwrap_or(&params.target),
            )
            .map_err(task_error)?;
        let job = match self.tasks().map_err(task_error)?.bind_job_runtime(
            &job.id,
            &params.target,
            &agent.terminal_id,
        ) {
            Ok(job) => job,
            Err(error) => {
                self.tasks()
                    .map_err(task_error)?
                    .interrupt_job(&job.id, &error)
                    .map_err(task_error)?;
                return Err(task_error(error));
            }
        };
        let task = self
            .tasks()
            .map_err(task_error)?
            .data()
            .tasks
            .get(&params.task_id)
            .cloned()
            .ok_or_else(|| task_error("task not found".into()))?;
        let text = format!("{}\n\n{}\n\nAcceptance criteria:\n{}\n\nThis is Herdr job {}. When finished, report this exact job using the Herdr CLI, with your result, test evidence, and artifacts:\nherdr job report {} --token {} --status succeeded --result <summary>\nUse --status failed if the task failed. Do not report success merely because work stopped.\n", task.title, task.brief.clone() + &previous_context, task.criteria.join("\n"), job.id, job.id, job.report_token);
        match self.queue_tracked_prompt(params.target, text) {
            Ok(receiver) => {
                self.task_submissions.insert(job.id.clone(), receiver);
            }
            Err(error) => {
                self.tasks()
                    .map_err(task_error)?
                    .interrupt_job(&job.id, &error.message)
                    .map_err(task_error)?;
                return Err(ErrorBody { code: error.code, message: format!("{}; job {} retained for inspection; do not retry with a new key until inspected", error.message, job.id) });
            }
        }
        Ok(ResponseResult::JobInfo { job })
    }

    /// Run Git outside the application loop so a large diff cannot freeze terminals.
    pub(crate) fn handle_deferred_task_diff(
        &mut self,
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let Method::TaskDiff(params) = request.method else {
            return;
        };
        let cwd = self.tasks().and_then(|store| {
            store
                .data()
                .tasks
                .get(&params.task_id)
                .and_then(|task| task.cwd.clone())
                .ok_or_else(|| "task has no working directory".into())
        });
        let cwd = match cwd {
            Ok(cwd) => cwd,
            Err(error) => {
                let _ = respond_to.send(encode_error(request.id, "task_diff_unavailable", error));
                return;
            }
        };
        let failed_to = respond_to.clone();
        let failed_id = request.id.clone();
        if let Err(error) = crate::thread_spawn::spawn_named("herdr-task-diff", move || {
            let result = working_diff(&cwd);
            let response = match result {
                Ok(diff) => encode_success(request.id, ResponseResult::TaskDiff { diff }),
                Err(error) => encode_error(request.id, "task_diff_failed", error),
            };
            let _ = respond_to.send(response);
        }) {
            let _ = failed_to.send(encode_error(
                failed_id,
                "task_diff_failed",
                error.to_string(),
            ));
        }
    }
}

fn task_error(message: String) -> ErrorBody {
    ErrorBody {
        code: "task_error".into(),
        message,
    }
}

fn working_diff(cwd: &str) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    if !std::path::Path::new(cwd).is_absolute() {
        return Err("task working directory must be absolute".into());
    }
    let mut child = Command::new("git")
        .args([
            "-C",
            cwd,
            "--no-pager",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "HEAD",
            "--",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut output = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(512 * 1024 + 1)
            .read_to_end(&mut output)
            .map_err(|e| e.to_string())?;
    }
    if output.len() > 512 * 1024 {
        let _ = child.kill();
        let _ = child.wait();
        return Err("diff exceeds 512 KiB; inspect it in the task working directory".into());
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(
            "git diff failed; task directory must be a repository with a HEAD commit".into(),
        );
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{CreateTask, JobReport, JobState, ReviewDecision};

    fn app() -> App {
        let (_, rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            rx,
            crate::api::EventHub::default(),
        )
    }
    fn call(app: &mut App, method: Method) -> serde_json::Value {
        serde_json::from_str(&app.handle_api_request(Request {
            id: "test".into(),
            method,
        }))
        .unwrap()
    }
    #[tokio::test]
    async fn task_api_keeps_job_results_and_reviews_separate_from_terminal_state() {
        let mut app = app();
        let created = call(
            &mut app,
            Method::TaskCreate(CreateTask {
                title: "Login fix".into(),
                brief: "Preserve the redirect".into(),
                ..Default::default()
            }),
        );
        let task_id = created["result"]["task"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let job = app
            .tasks()
            .unwrap()
            .start_job(&task_id, "once", "w1:p1", "codex")
            .unwrap();
        app.tasks()
            .unwrap()
            .observe_agent_state("w1:p1", "idle")
            .unwrap();
        let read = call(
            &mut app,
            Method::JobGet(JobTarget {
                job_id: job.id.clone(),
            }),
        );
        assert_eq!(read["result"]["job"]["state"], "running");
        let bad = call(
            &mut app,
            Method::JobReport(JobReportParams {
                job_id: job.id.clone(),
                token: "wrong".into(),
                report: JobReport {
                    state: JobState::Succeeded,
                    results: "fixed".into(),
                    tests: "cargo test".into(),
                },
            }),
        );
        assert!(bad.get("error").is_some());
        let reported = call(
            &mut app,
            Method::JobReport(JobReportParams {
                job_id: job.id.clone(),
                token: job.report_token,
                report: JobReport {
                    state: JobState::Succeeded,
                    results: "fixed redirect".into(),
                    tests: "cargo test passed".into(),
                },
            }),
        );
        assert_eq!(reported["result"]["job"]["state"], "succeeded");
        let summary = call(
            &mut app,
            Method::TaskSummary(TaskSummaryParams {
                consumer: "reviewer".into(),
            }),
        );
        assert_eq!(
            summary["result"]["summary"]["needs_review"][0]["id"],
            task_id
        );
        let accepted = call(
            &mut app,
            Method::TaskReview(TaskReviewParams {
                task_id,
                decision: ReviewDecision::Accept,
                note: "Verified redirect".into(),
            }),
        );
        assert_eq!(accepted["result"]["task"]["status"], "accepted");
        let found = call(
            &mut app,
            Method::TaskActivity(TaskActivityParams {
                query: Some("redirect".into()),
                after: 0,
                limit: 100,
            }),
        );
        assert!(!found["result"]["activities"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn duplicate_job_start_never_needs_a_live_agent_or_resubmits() {
        let mut app = app();
        let task = app
            .tasks()
            .unwrap()
            .create_task(CreateTask {
                title: "Task".into(),
                ..Default::default()
            })
            .unwrap();
        let job = app
            .tasks()
            .unwrap()
            .start_job(&task.id, "dedupe", "w1:p1", "codex")
            .unwrap();
        let response = call(
            &mut app,
            Method::JobStart(JobStartParams {
                task_id: task.id,
                request_key: "dedupe".into(),
                target: "codex".into(),
            }),
        );
        assert_eq!(response["result"]["job"]["id"], job.id);
        assert!(app.task_submissions.is_empty());
    }
    #[tokio::test]
    async fn rejected_job_does_not_reserve_or_write_to_a_missing_agent() {
        let mut app = app();
        let task = app
            .tasks()
            .unwrap()
            .create_task(CreateTask {
                title: "Task".into(),
                ..Default::default()
            })
            .unwrap();
        let response = call(
            &mut app,
            Method::JobStart(JobStartParams {
                task_id: task.id,
                request_key: "missing".into(),
                target: "missing".into(),
            }),
        );
        assert!(response.get("error").is_some());
        assert!(app.tasks().unwrap().data().jobs.is_empty());
    }
    #[tokio::test]
    async fn delivery_failure_preserves_uncertainty_and_does_not_retry() {
        let mut app = app();
        let task = app
            .tasks()
            .unwrap()
            .create_task(CreateTask {
                title: "Task".into(),
                ..Default::default()
            })
            .unwrap();
        let job = app
            .tasks()
            .unwrap()
            .start_job(&task.id, "delivery", "w1:p1", "codex")
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        app.task_submissions.insert(job.id.clone(), rx);
        tx.send(Err(std::io::Error::other("write failed"))).unwrap();
        let response = call(&mut app, Method::JobGet(JobTarget { job_id: job.id }));
        assert_eq!(response["result"]["job"]["state"], "interrupted");
        assert!(app.task_submissions.is_empty());
    }
    #[tokio::test]
    async fn tracked_jobs_submit_once_with_report_token_and_reject_busy_agents() {
        let mut app = app();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("tracked")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_agent_name("coder".into());
        terminal.set_detected_state(
            Some(crate::detect::Agent::OpenCode),
            crate::detect::AgentState::Idle,
        );
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 8,
            );
        app.state.insert_test_runtime(pane_id, runtime);
        let task = app
            .tasks()
            .unwrap()
            .create_task(CreateTask {
                title: "Fix auth".into(),
                criteria: vec!["Redirect preserved".into()],
                ..Default::default()
            })
            .unwrap();
        let params = JobStartParams {
            task_id: task.id.clone(),
            request_key: "launch-once".into(),
            target: "coder".into(),
        };
        let first = call(&mut app, Method::JobStart(params.clone()));
        assert!(first.get("error").is_none(), "{first}");
        let again = call(&mut app, Method::JobStart(params));
        assert_eq!(first["result"]["job"]["id"], again["result"]["job"]["id"]);
        let prompt = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("tracked prompt was not delivered")
            .expect("tracked prompt channel closed");
        let prompt = String::from_utf8_lossy(&prompt);
        assert!(prompt.contains("Redirect preserved"));
        assert!(prompt.contains(first["result"]["job"]["report_token"].as_str().unwrap()));
        assert!(prompt.contains("herdr job report"));
        let enter = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("tracked Enter was not delivered")
            .expect("tracked prompt channel closed before Enter");
        assert!(!enter.is_empty());
        assert!(
            rx.try_recv().is_err(),
            "duplicate launch submitted extra terminal input"
        );
        let wrong = call(
            &mut app,
            Method::JobStart(JobStartParams {
                task_id: task.id,
                request_key: "launch-once".into(),
                target: "different-agent".into(),
            }),
        );
        assert!(wrong.get("error").is_some());
        let other = app
            .tasks()
            .unwrap()
            .create_task(CreateTask {
                title: "Second".into(),
                ..Default::default()
            })
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::OpenCode),
                crate::detect::AgentState::Working,
            );
        let busy = call(
            &mut app,
            Method::JobStart(JobStartParams {
                task_id: other.id,
                request_key: "busy".into(),
                target: "coder".into(),
            }),
        );
        assert_eq!(busy["error"]["code"], "agent_busy");
        assert_eq!(app.tasks().unwrap().data().jobs.len(), 1);
    }

    #[tokio::test]
    async fn workflow_api_returns_ready_steps_and_freezes_template() {
        let mut app = app();
        let response = call(
            &mut app,
            Method::WorkflowStart(WorkflowStartParams {
                template_id: "implement-test-review".into(),
                brief: "Fix login".into(),
                cwd: None,
            }),
        );
        let run_id = response["result"]["run"]["id"].as_str().unwrap().to_owned();
        assert_eq!(response["result"]["ready_steps"][0]["id"], "implement");
        let invalid = call(
            &mut app,
            Method::WorkflowStepStart(WorkflowStepStartParams {
                run_id: run_id.clone(),
                step_id: "review".into(),
            }),
        );
        assert!(invalid.get("error").is_some());
        let step = call(
            &mut app,
            Method::WorkflowStepStart(WorkflowStepStartParams {
                run_id,
                step_id: "implement".into(),
            }),
        );
        assert_eq!(step["result"]["task"]["status"], "ready");
    }
}
