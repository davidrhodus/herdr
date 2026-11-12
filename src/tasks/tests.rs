use super::*;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("herdr-task-store-{}-{time}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self) -> PathBuf {
        self.0.join("tasks.json")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn create(store: &mut TaskStore) -> Task {
    store
        .create_task(CreateTask {
            title: "Fix login".into(),
            brief: "Preserve the destination".into(),
            criteria: vec!["Redirect test passes".into()],
            branch: Some("fix/login".into()),
            ..CreateTask::default()
        })
        .unwrap()
}
fn report(store: &mut TaskStore, job: &Job, state: JobState) -> Job {
    store
        .report_job(
            &job.id,
            &job.report_token,
            JobReport {
                state,
                results: "Changed auth.rs".into(),
                tests: "cargo test redirect: passed".into(),
            },
        )
        .unwrap()
}
fn complete(store: &mut TaskStore, task: &Task, key: &str) {
    let job = store.start_job(&task.id, key, "w1:p1", "codex").unwrap();
    report(store, &job, JobState::Succeeded);
    store
        .review_task(&task.id, ReviewDecision::Accept, "Reviewed diff and tests")
        .unwrap();
}

#[test]
fn durable_tasks_survive_restart_and_pending_jobs_are_never_replayed() {
    let scratch = Scratch::new();
    let mut store = TaskStore::open(scratch.file()).unwrap();
    let task = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    drop(store);
    let store = TaskStore::open(scratch.file()).unwrap();
    assert_eq!(store.data.jobs[&job.id].state, JobState::Interrupted);
    assert_eq!(store.data.tasks[&task.id].status, TaskStatus::Blocked);
    assert_eq!(store.data.jobs.len(), 1);
    assert_eq!(
        store.data.tasks[&task.id].criteria,
        vec!["Redirect test passes"]
    );
    assert!(store
        .summary("david")
        .failed_jobs
        .iter()
        .any(|j| j.id == job.id));
    let count = store.data.activities.len();
    drop(store);
    assert_eq!(
        TaskStore::open(scratch.file())
            .unwrap()
            .data
            .activities
            .len(),
        count
    );
}

#[test]
fn corrupt_and_future_version_stores_are_preserved() {
    let scratch = Scratch::new();
    for bytes in [
        b"{broken".to_vec(),
        serde_json::to_vec(&TaskData {
            version: 999,
            ..TaskData::default()
        })
        .unwrap(),
    ] {
        std::fs::write(scratch.file(), &bytes).unwrap();
        assert!(TaskStore::open(scratch.file()).is_err());
        assert_eq!(std::fs::read(scratch.file()).unwrap(), bytes);
    }
}

#[test]
fn failed_save_keeps_memory_and_original_bytes() {
    let scratch = Scratch::new();
    let mut store = TaskStore::open(scratch.file()).unwrap();
    create(&mut store);
    let before = serde_json::to_value(store.data()).unwrap();
    // A directory at the destination makes atomic replacement fail on all platforms.
    let original = std::fs::read(scratch.file()).unwrap();
    std::fs::remove_file(scratch.file()).unwrap();
    std::fs::create_dir(scratch.file()).unwrap();
    assert!(store
        .create_task(CreateTask {
            title: "Second task".into(),
            ..CreateTask::default()
        })
        .is_err());
    assert_eq!(serde_json::to_value(store.data()).unwrap(), before);
    std::fs::remove_dir(scratch.file()).unwrap();
    std::fs::write(scratch.file(), original).unwrap();
    assert_eq!(TaskStore::open(scratch.file()).unwrap().data.tasks.len(), 1);
}

#[test]
fn job_identity_retries_and_explicit_reports_are_bound_to_one_task() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    let other = create(&mut store);
    let job = store
        .start_job(&task.id, "retry-key", "w1:p1", "codex")
        .unwrap();
    let count = store.data.activities.len();
    assert_eq!(
        store
            .start_job(&task.id, "retry-key", "w1:p1", "codex")
            .unwrap(),
        job
    );
    assert_eq!(store.data.activities.len(), count);
    assert!(store
        .start_job(&other.id, "retry-key", "w1:p2", "codex")
        .is_err());
    assert!(store
        .start_job(&other.id, "other-key", "w1:p1", "codex")
        .is_err());
    assert!(store
        .start_job(&task.id, "other-key", "w1:p2", "codex")
        .is_err());
    assert!(store
        .report_job(
            &job.id,
            "wrong token",
            JobReport {
                state: JobState::Succeeded,
                results: "ok".into(),
                tests: "".into()
            }
        )
        .is_err());
    store.observe_agent_state("w1:p1", "working").unwrap();
    store.observe_agent_state("w1:p1", "done").unwrap();
    assert_eq!(store.data.tasks[&task.id].status, TaskStatus::Running);
    assert_eq!(store.data.jobs[&job.id].state, JobState::Running);
    report(&mut store, &job, JobState::Succeeded);
    assert_eq!(store.data.tasks[&task.id].status, TaskStatus::Review);
    let count = store.data.activities.len();
    report(&mut store, &job, JobState::Succeeded);
    assert_eq!(store.data.activities.len(), count);
    assert!(store
        .report_job(
            &job.id,
            &job.report_token,
            JobReport {
                state: JobState::Failed,
                results: "changed my mind".into(),
                tests: "".into()
            }
        )
        .is_err());
    store
        .review_task(
            &task.id,
            ReviewDecision::RequestChanges,
            "Handle empty destination",
        )
        .unwrap();
    assert_eq!(
        store.data.tasks[&task.id].status,
        TaskStatus::ChangesRequested
    );
    let retry = store
        .start_job(&task.id, "new-attempt", "w1:p1", "codex")
        .unwrap();
    assert_ne!(retry.report_token, job.report_token);
    assert_eq!(store.data.tasks[&task.id].review, None);
}

#[test]
fn reports_cannot_claim_untracked_states_or_empty_success() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    for state in [
        JobState::Running,
        JobState::Interrupted,
        JobState::Cancelled,
        JobState::Succeeded,
    ] {
        assert!(store
            .report_job(
                &job.id,
                &job.report_token,
                JobReport {
                    state,
                    results: "".into(),
                    tests: "".into()
                }
            )
            .is_err());
    }
    assert!(store
        .review_task(&task.id, ReviewDecision::Accept, "")
        .is_err());
    assert!(store
        .update_task(
            &task.id,
            &TaskPatch {
                status: Some(TaskStatus::Accepted),
                ..TaskPatch::default()
            }
        )
        .is_err());
}

#[test]
fn blocked_reports_can_resume_and_interruption_releases_reservation() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    report(&mut store, &job, JobState::Blocked);
    assert_eq!(store.data.tasks[&task.id].status, TaskStatus::Blocked);
    assert!(store.start_job(&task.id, "two", "w1:p1", "codex").is_err());
    report(&mut store, &job, JobState::Succeeded);
    store
        .review_task(&task.id, ReviewDecision::RequestChanges, "More tests")
        .unwrap();
    let second = store.start_job(&task.id, "two", "w1:p1", "codex").unwrap();
    store
        .interrupt_job(&second.id, "submission acknowledgement lost")
        .unwrap();
    let third = store
        .start_job(&task.id, "three", "w1:p1", "codex")
        .unwrap();
    assert_ne!(second.id, third.id);
}

#[test]
fn observations_are_deduplicated_and_agent_exit_is_uncertain() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    store.observe_agent_state("w1:p1", "blocked").unwrap();
    let count = store.data.activities.len();
    store.observe_agent_state("w1:p1", "blocked").unwrap();
    store.observe_agent_state("w9:p9", "done").unwrap();
    assert_eq!(store.data.activities.len(), count);
    assert_eq!(store.data.tasks[&task.id].status, TaskStatus::Blocked);
    store.observe_agent_state("w1:p1", "working").unwrap();
    assert_eq!(store.data.tasks[&task.id].status, TaskStatus::Running);
    store.observe_agent_state("w1:p1", "exited").unwrap();
    assert_eq!(store.data.jobs[&job.id].state, JobState::Interrupted);
}

#[test]
fn durable_activity_search_and_independent_acknowledgements() {
    let scratch = Scratch::new();
    let mut store = TaskStore::open(scratch.file()).unwrap();
    let task = create(&mut store);
    complete(&mut store, &task, "one");
    let summary = store.summary("david");
    assert_eq!(store.summary("david"), summary);
    assert!(store
        .activity(Some("AUTH.RS"), 0, 50)
        .iter()
        .any(|a| a.task_id.as_deref() == Some(&task.id)));
    let first_page = store.activity(None, 0, 1);
    assert_eq!(first_page.len(), 1);
    let rest = store.activity(None, first_page[0].seq, 1000);
    assert_eq!(rest.len() + 1, summary.activities.len());
    store.acknowledge("david", summary.through).unwrap();
    assert!(store.summary("david").activities.is_empty());
    assert!(!store.summary("codex").activities.is_empty());
    assert!(store.acknowledge("david", summary.through + 1000).is_err());
    store.acknowledge("david", 0).unwrap();
    drop(store);
    assert!(TaskStore::open(scratch.file())
        .unwrap()
        .summary("david")
        .activities
        .is_empty());
}

#[test]
fn workflow_dependencies_require_review_acceptance_and_snapshot_template() {
    let mut store = TaskStore::memory();
    let cwd = std::env::temp_dir()
        .join("herdr-worktree")
        .display()
        .to_string();
    let run = store
        .start_workflow("implement-test-review", "Fix login", Some(cwd.clone()))
        .unwrap();
    assert_eq!(
        store
            .ready_steps(&run.id)
            .unwrap()
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        vec!["implement"]
    );
    assert!(store.start_step(&run.id, "test").is_err());
    let implement = store.start_step(&run.id, "implement").unwrap();
    assert_eq!(implement.cwd.as_deref(), Some(cwd.as_str()));
    assert_eq!(store.start_step(&run.id, "implement").unwrap(), implement);
    let job = store
        .start_job(&implement.id, "one", "w1:p1", "codex")
        .unwrap();
    report(&mut store, &job, JobState::Succeeded);
    assert!(store.start_step(&run.id, "test").is_err());
    store
        .review_task(&implement.id, ReviewDecision::Accept, "Reviewed")
        .unwrap();
    let test = store.start_step(&run.id, "test").unwrap();
    assert!(test.brief.contains("Changed auth.rs"));
    assert!(test.brief.contains("Fix login"));
    let job = store.start_job(&test.id, "two", "w1:p1", "codex").unwrap();
    report(&mut store, &job, JobState::Failed);
    assert!(store.start_step(&run.id, "review").is_err());
    let mut updated = default_workflow();
    updated.steps[0].prompt = "New template prompt".into();
    store.save_template(updated).unwrap();
    assert!(!store.workflow_run(&run.id).unwrap().template.steps[0]
        .prompt
        .contains("New template"));
}

#[test]
fn invalid_dags_do_not_change_the_store() {
    let mut store = TaskStore::memory();
    let count = store.data.templates.len();
    for mutate in [0, 1, 2, 3] {
        let mut template = default_workflow();
        template.id = format!("invalid-{mutate}");
        match mutate {
            0 => template.steps[0].depends_on = vec!["review".into()],
            1 => template.steps[0].depends_on = vec!["missing".into()],
            2 => template.steps[1].id = "implement".into(),
            _ => template.steps.clear(),
        }
        assert!(store.save_template(template).is_err());
        assert_eq!(store.data.templates.len(), count);
    }
}

#[test]
fn cancelled_tracking_never_claims_the_process_was_stopped() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    let cancelled = store.cancel_job(&job.id).unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert_eq!(store.cancel_job(&job.id).unwrap(), cancelled);
    assert!(store
        .data
        .activities
        .last()
        .unwrap()
        .text
        .contains("terminal process is unchanged"));
}

#[test]
fn latest_interrupted_job_can_be_resolved_but_stale_reports_cannot_replace_retry() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    store.interrupt_job(&job.id, "channel closed").unwrap();
    report(&mut store, &job, JobState::Succeeded);
    assert_eq!(store.data.tasks[&task.id].status, TaskStatus::Review);
    store
        .review_task(&task.id, ReviewDecision::RequestChanges, "More tests")
        .unwrap();
    let second = store.start_job(&task.id, "two", "w1:p1", "codex").unwrap();
    store.interrupt_job(&second.id, "server restarted").unwrap();
    let third = store
        .start_job(&task.id, "three", "w1:p1", "codex")
        .unwrap();
    assert!(store
        .report_job(
            &second.id,
            &second.report_token,
            JobReport {
                state: JobState::Succeeded,
                results: "Old answer".into(),
                tests: "".into()
            }
        )
        .is_err());
    assert_eq!(store.data.tasks[&task.id].status, TaskStatus::Running);
    assert_eq!(store.data.tasks[&task.id].job_ids.last(), Some(&third.id));
}

#[test]
fn interrupted_blocked_job_cannot_steal_another_active_job_pane() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    let other = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    store.interrupt_job(&job.id, "channel closed").unwrap();
    store.start_job(&other.id, "two", "w1:p1", "codex").unwrap();
    assert!(store
        .report_job(
            &job.id,
            &job.report_token,
            JobReport {
                state: JobState::Blocked,
                results: "Need input".into(),
                tests: "".into()
            }
        )
        .is_err());
    report(&mut store, &job, JobState::Succeeded);
    assert_eq!(store.data.tasks[&task.id].status, TaskStatus::Review);
    assert_eq!(store.data.tasks[&other.id].status, TaskStatus::Running);
}

#[test]
fn physical_terminal_identity_survives_pane_moves_and_prevents_parallel_jobs() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    let other = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    let bound = store.bind_job_runtime(&job.id, "codex", "term-1").unwrap();
    assert_eq!(bound.launch_target.as_deref(), Some("codex"));
    store
        .observe_terminal_state("term-1", "w2:p9", "working")
        .unwrap();
    assert_eq!(store.data.jobs[&job.id].pane_id, "w2:p9");
    assert_eq!(store.data.tasks[&task.id].pane_id.as_deref(), Some("w2:p9"));
    let second = store.start_job(&other.id, "two", "w1:p1", "codex").unwrap();
    assert!(store
        .bind_job_runtime(&second.id, "codex", "term-1")
        .is_err());
    assert!(store
        .bind_job_runtime(&job.id, "other-agent", "term-1")
        .is_err());
    assert!(store.bind_job_runtime(&job.id, "codex", "term-2").is_err());
    store
        .observe_terminal_state("term-1", "w2:p9", "exited")
        .unwrap();
    assert_eq!(store.data.jobs[&job.id].state, JobState::Interrupted);
    assert_eq!(store.data.jobs[&second.id].state, JobState::Running);
}

#[test]
fn activity_preserves_original_brief_after_task_edits() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    store
        .update_task(
            &task.id,
            &TaskPatch {
                brief: Some("Replace the old destination logic".into()),
                ..TaskPatch::default()
            },
        )
        .unwrap();
    let original = store.activity(Some("Preserve the destination"), 0, 100);
    assert_eq!(original.len(), 1);
    assert_eq!(original[0].kind, "task.created");
    let updated = store.activity(Some("Replace the old destination"), 0, 100);
    assert_eq!(updated.len(), 1);
    assert_eq!(updated[0].kind, "task.updated");
}

#[test]
fn damaged_identity_counters_and_references_are_refused_before_mutation() {
    let scratch = Scratch::new();
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    let mut data = store.data.clone();
    data.next_id = 1;
    let bytes = serde_json::to_vec(&data).unwrap();
    std::fs::write(scratch.file(), &bytes).unwrap();
    assert!(TaskStore::open(scratch.file()).is_err());
    assert_eq!(std::fs::read(scratch.file()).unwrap(), bytes);
    let mut data = store.data.clone();
    data.tasks.get_mut(&task.id).unwrap().job_ids.clear();
    std::fs::write(scratch.file(), serde_json::to_vec(&data).unwrap()).unwrap();
    assert!(TaskStore::open(scratch.file()).is_err());
}

#[test]
fn immutable_review_contract_and_active_provenance_are_protected() {
    let mut store = TaskStore::memory();
    assert!(store
        .create_task(CreateTask {
            title: "Relative".into(),
            cwd: Some("relative/path".into()),
            ..CreateTask::default()
        })
        .is_err());
    let task = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    for patch in [
        TaskPatch {
            title: Some("Different goal".into()),
            ..TaskPatch::default()
        },
        TaskPatch {
            cwd: Some("/other".into()),
            ..TaskPatch::default()
        },
        TaskPatch {
            agent: Some("reviewer".into()),
            ..TaskPatch::default()
        },
        TaskPatch {
            branch: Some("different-branch".into()),
            ..TaskPatch::default()
        },
    ] {
        assert!(store.update_task(&task.id, &patch).is_err());
    }
    report(&mut store, &job, JobState::Succeeded);
    assert!(store
        .update_task(
            &task.id,
            &TaskPatch {
                criteria: Some(vec!["New criteria".into()]),
                ..TaskPatch::default()
            }
        )
        .is_err());
    store
        .review_task(
            &task.id,
            ReviewDecision::RequestChanges,
            "Need a revised goal",
        )
        .unwrap();
    store
        .update_task(
            &task.id,
            &TaskPatch {
                brief: Some("Revised goal".into()),
                ..TaskPatch::default()
            },
        )
        .unwrap();
    let job = store.start_job(&task.id, "two", "w1:p1", "codex").unwrap();
    report(&mut store, &job, JobState::Succeeded);
    store
        .review_task(&task.id, ReviewDecision::Accept, "Done")
        .unwrap();
    assert!(store
        .update_task(
            &task.id,
            &TaskPatch {
                brief: Some("Another goal".into()),
                ..TaskPatch::default()
            }
        )
        .is_err());
}

#[test]
fn return_summary_does_not_present_resolved_attempts_as_current_failures() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    report(&mut store, &job, JobState::Failed);
    assert_eq!(store.summary("david").failed_jobs.len(), 1);
    let retry = store.start_job(&task.id, "two", "w1:p1", "codex").unwrap();
    report(&mut store, &retry, JobState::Succeeded);
    store
        .review_task(&task.id, ReviewDecision::Accept, "Verified")
        .unwrap();
    assert!(store.summary("david").failed_jobs.is_empty());
    assert!(!store.activity(Some("Failed"), 0, 100).is_empty());
}

#[test]
fn completed_job_follows_its_terminal_before_review_without_reopening_work() {
    let mut store = TaskStore::memory();
    let task = create(&mut store);
    let job = store.start_job(&task.id, "one", "w1:p1", "codex").unwrap();
    store
        .bind_job_runtime(&job.id, "codex", "terminal-1")
        .unwrap();
    report(&mut store, &job, JobState::Succeeded);
    store.relocate_terminal("terminal-1", "w3:p7").unwrap();
    assert_eq!(store.data.jobs[&job.id].pane_id, "w3:p7");
    assert_eq!(store.data.jobs[&job.id].state, JobState::Succeeded);
    assert_eq!(store.data.tasks[&task.id].pane_id.as_deref(), Some("w3:p7"));
    assert_eq!(store.data.tasks[&task.id].status, TaskStatus::Review);
    let count = store.data.activities.len();
    store.relocate_terminal("terminal-1", "w3:p7").unwrap();
    store.relocate_terminal("missing", "w3:p8").unwrap();
    assert_eq!(store.data.activities.len(), count);
    store
        .review_task(&task.id, ReviewDecision::RequestChanges, "More tests")
        .unwrap();
    let newer = store
        .start_job(&task.id, "two", "w9:p2", "reviewer")
        .unwrap();
    store
        .bind_job_runtime(&newer.id, "reviewer", "terminal-2")
        .unwrap();
    store.relocate_terminal("terminal-1", "w4:p8").unwrap();
    assert_eq!(store.data.jobs[&job.id].pane_id, "w4:p8");
    assert_eq!(store.data.tasks[&task.id].pane_id.as_deref(), Some("w9:p2"));
}
