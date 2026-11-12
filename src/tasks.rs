//! Durable work records. Terminal lifecycle is never evidence that a job succeeded.
//! The server owns this store; clients read projections and submit explicit reports.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Ready,
    Running,
    Blocked,
    Review,
    ChangesRequested,
    Accepted,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Blocked,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}
impl JobState {
    pub fn active(self) -> bool {
        matches!(self, Self::Running | Self::Blocked)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Accept,
    RequestChanges,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateTask {
    pub title: String,
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub criteria: Vec<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub pane_id: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskPatch {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub brief: Option<String>,
    #[serde(default)]
    pub criteria: Option<Vec<String>>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub pane_id: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Review {
    pub decision: ReviewDecision,
    pub note: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub brief: String,
    pub criteria: Vec<String>,
    pub agent: Option<String>,
    pub pane_id: Option<String>,
    pub branch: Option<String>,
    pub cwd: Option<String>,
    pub status: TaskStatus,
    pub results: String,
    /// Reported test evidence, not inferred from terminal output.
    pub tests: String,
    pub review: Option<Review>,
    pub job_ids: Vec<String>,
    pub workflow_run_id: Option<String>,
    pub workflow_step_id: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobReport {
    pub state: JobState,
    #[serde(default)]
    pub results: String,
    #[serde(default)]
    pub tests: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Job {
    pub id: String,
    pub task_id: String,
    pub request_key: String,
    /// Correlation token, not an authorization boundary. The local socket remains trusted.
    pub report_token: String,
    pub pane_id: String,
    pub agent: String,
    #[serde(default)]
    pub launch_target: Option<String>,
    #[serde(default)]
    pub terminal_id: Option<String>,
    pub state: JobState,
    #[serde(default)]
    pub observed_state: Option<String>,
    pub report: Option<JobReport>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowStep {
    pub id: String,
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub criteria: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowTemplate {
    pub id: String,
    pub title: String,
    pub steps: Vec<WorkflowStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowRun {
    pub id: String,
    /// Snapshot the template so subsequent template edits do not alter running work.
    pub template: WorkflowTemplate,
    pub brief: String,
    pub cwd: Option<String>,
    pub tasks: BTreeMap<String, String>,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Activity {
    pub seq: u64,
    pub at_ms: u64,
    pub kind: String,
    pub task_id: Option<String>,
    pub job_id: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReturnSummary {
    pub consumer: String,
    pub after: u64,
    pub through: u64,
    pub activities: Vec<Activity>,
    pub needs_review: Vec<Task>,
    pub blocked: Vec<Task>,
    pub failed_jobs: Vec<Job>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskData {
    pub version: u32,
    pub next_id: u64,
    pub next_seq: u64,
    pub tasks: BTreeMap<String, Task>,
    pub jobs: BTreeMap<String, Job>,
    pub templates: BTreeMap<String, WorkflowTemplate>,
    pub workflows: BTreeMap<String, WorkflowRun>,
    pub activities: Vec<Activity>,
    pub cursors: BTreeMap<String, u64>,
}

impl Default for TaskData {
    fn default() -> Self {
        let template = default_workflow();
        Self {
            version: 1,
            next_id: 1,
            next_seq: 1,
            tasks: BTreeMap::new(),
            jobs: BTreeMap::new(),
            templates: BTreeMap::from([(template.id.clone(), template)]),
            workflows: BTreeMap::new(),
            activities: Vec::new(),
            cursors: BTreeMap::new(),
        }
    }
}

pub fn default_workflow() -> WorkflowTemplate {
    WorkflowTemplate { id: "implement-test-review".into(), title: "Implement, test, review".into(), steps: vec![
        WorkflowStep { id: "implement".into(), title: "Implement".into(), prompt: "Implement the requested change. Report changed files and reasoning.".into(), agent: None, depends_on: vec![], criteria: vec!["Requested behavior implemented".into()] },
        WorkflowStep { id: "test".into(), title: "Test".into(), prompt: "Validate the implementation. Run relevant checks and report exact commands and outcomes.".into(), agent: None, depends_on: vec!["implement".into()], criteria: vec!["Relevant checks pass".into()] },
        WorkflowStep { id: "review".into(), title: "Review".into(), prompt: "Review the implementation and validation. Report actionable findings and remaining risks.".into(), agent: None, depends_on: vec!["test".into()], criteria: vec!["Review findings documented".into()] },
    ] }
}

/// One server serializes all access; no filesystem work belongs in render loops.
pub struct TaskStore {
    path: Option<PathBuf>,
    data: TaskData,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

fn nonempty(value: &str, name: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > max || value.contains('\0') {
        return Err(format!(
            "{name} must contain visible text and be at most {max} bytes"
        ));
    }
    Ok(())
}

fn identifier(value: &str, name: &str) -> Result<(), String> {
    nonempty(value, name, 128)?;
    if !value
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return Err(format!(
            "{name} must use letters, digits, hyphens, or underscores"
        ));
    }
    Ok(())
}

impl TaskData {
    fn id(&mut self, prefix: &str) -> Result<String, String> {
        let id = format!("{prefix}-{}", self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("task ID space exhausted")?;
        Ok(id)
    }
    fn record(
        &mut self,
        kind: &str,
        task_id: Option<String>,
        job_id: Option<String>,
        text: String,
    ) -> Result<(), String> {
        let seq = self.next_seq;
        self.next_seq = seq.checked_add(1).ok_or("activity sequence exhausted")?;
        self.activities.push(Activity {
            seq,
            at_ms: now_ms(),
            kind: kind.into(),
            task_id,
            job_id,
            text,
        });
        Ok(())
    }
    fn create(&mut self, request: CreateTask) -> Result<Task, String> {
        validate_create(&request)?;
        let id = self.id("task")?;
        let time = now_ms();
        let task = Task {
            id: id.clone(),
            title: request.title,
            brief: request.brief,
            criteria: request.criteria,
            agent: request.agent,
            pane_id: request.pane_id,
            branch: request.branch,
            cwd: request.cwd,
            status: TaskStatus::Ready,
            results: String::new(),
            tests: String::new(),
            review: None,
            job_ids: vec![],
            workflow_run_id: None,
            workflow_step_id: None,
            created_at_ms: time,
            updated_at_ms: time,
        };
        self.record(
            "task.created",
            Some(id.clone()),
            None,
            task_description(&task),
        )?;
        self.tasks.insert(id, task.clone());
        Ok(task)
    }
}

fn task_description(task: &Task) -> String {
    format!(
        "{}\n{}\nAcceptance criteria:\n{}\nBranch: {}\nDirectory: {}",
        task.title,
        task.brief,
        task.criteria.join("\n"),
        task.branch.as_deref().unwrap_or(""),
        task.cwd.as_deref().unwrap_or("")
    )
}

fn validate_create(request: &CreateTask) -> Result<(), String> {
    nonempty(&request.title, "title", 240)?;
    if request.brief.len() > 128 * 1024
        || request.criteria.len() > 100
        || request.criteria.iter().any(|s| s.len() > 8192)
    {
        return Err("task brief or criteria exceeds limits".into());
    }
    if request
        .cwd
        .as_ref()
        .is_some_and(|cwd| !Path::new(cwd).is_absolute())
    {
        return Err("task working directory must be absolute".into());
    }
    for (value, name) in [
        (&request.agent, "agent"),
        (&request.pane_id, "pane_id"),
        (&request.branch, "branch"),
        (&request.cwd, "cwd"),
    ] {
        if let Some(value) = value {
            nonempty(value, name, 8192)?;
        }
    }
    Ok(())
}

impl TaskStore {
    pub fn memory() -> Self {
        Self {
            path: None,
            data: TaskData::default(),
        }
    }
    /// Invalid or future-version stores are left untouched. Callers must surface the error.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<TaskData>(&bytes).map_err(|e| {
                format!(
                    "cannot load {}: {e}; original file preserved",
                    path.display()
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => TaskData::default(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        validate_data(&data)?;
        let mut store = Self {
            path: Some(path),
            data,
        };
        // A process restart cannot prove whether a terminal submission ran. Never replay it.
        if store.data.jobs.values().any(|j| j.state.active()) {
            store.transaction(|data| {
                let ids: Vec<_> = data
                    .jobs
                    .values()
                    .filter(|j| j.state.active())
                    .map(|j| j.id.clone())
                    .collect();
                for id in ids {
                    let (task_id, title) = {
                        let job = data.jobs.get_mut(&id).ok_or("missing interrupted job")?;
                        job.state = JobState::Interrupted;
                        job.updated_at_ms = now_ms();
                        (
                            job.task_id.clone(),
                            format!(
                                "{} interrupted by server restart; inspect before retrying",
                                job.id
                            ),
                        )
                    };
                    if let Some(task) = data.tasks.get_mut(&task_id) {
                        task.status = TaskStatus::Blocked;
                        task.updated_at_ms = now_ms();
                    }
                    data.record("job.interrupted", Some(task_id), Some(id), title)?;
                }
                Ok(())
            })?;
        }
        Ok(store)
    }
    pub fn data(&self) -> &TaskData {
        &self.data
    }
    fn transaction<T>(
        &mut self,
        change: impl FnOnce(&mut TaskData) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut candidate = self.data.clone();
        let result = change(&mut candidate)?;
        if let Some(path) = &self.path {
            save(path, &candidate)?;
        }
        self.data = candidate;
        Ok(result)
    }
    pub fn create_task(&mut self, request: CreateTask) -> Result<Task, String> {
        self.transaction(|data| data.create(request))
    }
    pub fn update_task(&mut self, id: &str, patch: &TaskPatch) -> Result<Task, String> {
        self.transaction(|data| {
            let active = data
                .jobs
                .values()
                .any(|j| j.task_id == id && j.state.active());
            let task = data.tasks.get_mut(id).ok_or("task not found")?;
            let content_change = patch.title.is_some() || patch.brief.is_some() || patch.criteria.is_some();
            if content_change && (active || matches!(task.status, TaskStatus::Review | TaskStatus::Accepted)) {
                return Err("task content cannot change while running or awaiting/completing review; request changes or create a follow-up task".into());
            }
            if active && (patch.agent.is_some() || patch.pane_id.is_some() || patch.cwd.is_some() || patch.branch.is_some()) {
                return Err("cannot change assignment or provenance while a task has an active job".into());
            }
            if let Some(status) = patch.status {
                if active {
                    return Err(
                        "cancel or report the active job before changing task status".into(),
                    );
                }
                if !matches!(status, TaskStatus::Ready | TaskStatus::Cancelled) {
                    return Err("task status must be changed through job reports or review".into());
                }
                if task.status == TaskStatus::Accepted && status == TaskStatus::Ready {
                    return Err("accepted tasks cannot be reopened; create a follow-up task".into());
                }
                task.status = status;
            }
            if let Some(v) = &patch.title {
                task.title = v.clone();
            }
            if let Some(v) = &patch.brief {
                task.brief = v.clone();
            }
            if let Some(v) = &patch.criteria {
                task.criteria = v.clone();
            }
            if let Some(v) = &patch.agent {
                task.agent = Some(v.clone());
            }
            if let Some(v) = &patch.pane_id {
                if active {
                    return Err("cannot reassign an active task".into());
                }
                task.pane_id = Some(v.clone());
            }
            if let Some(v) = &patch.branch {
                task.branch = Some(v.clone());
            }
            if let Some(v) = &patch.cwd {
                task.cwd = Some(v.clone());
            }
            validate_create(&CreateTask {
                title: task.title.clone(),
                brief: task.brief.clone(),
                criteria: task.criteria.clone(),
                agent: task.agent.clone(),
                pane_id: task.pane_id.clone(),
                branch: task.branch.clone(),
                cwd: task.cwd.clone(),
            })?;
            task.updated_at_ms = now_ms();
            let task = task.clone();
            data.record(
                "task.updated",
                Some(id.into()),
                None,
                task_description(&task),
            )?;
            Ok(task)
        })
    }
    pub fn start_job(
        &mut self,
        task_id: &str,
        request_key: &str,
        pane_id: &str,
        agent: &str,
    ) -> Result<Job, String> {
        nonempty(request_key, "request_key", 240)?;
        nonempty(pane_id, "pane_id", 240)?;
        nonempty(agent, "agent", 240)?;
        self.transaction(|data| {
            if let Some(job) = data.jobs.values().find(|j| j.request_key == request_key) {
                if job.task_id == task_id && job.pane_id == pane_id && job.agent == agent {
                    return Ok(job.clone());
                }
                return Err("request_key already used with different job parameters".into());
            }
            if data
                .jobs
                .values()
                .any(|j| j.state.active() && (j.pane_id == pane_id || j.task_id == task_id))
            {
                return Err("pane or task already has an active job".into());
            }
            let task = data.tasks.get(task_id).ok_or("task not found")?;
            if !matches!(
                task.status,
                TaskStatus::Ready | TaskStatus::ChangesRequested | TaskStatus::Blocked
            ) {
                return Err(
                    "task must be ready, blocked, or need changes before starting a job".into(),
                );
            }
            let id = data.id("job")?;
            let time = now_ms();
            let token_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|v| v.as_nanos())
                .unwrap_or(0);
            let job = Job {
                id: id.clone(),
                task_id: task_id.into(),
                request_key: request_key.into(),
                report_token: format!("{id}-{token_time:x}-{:x}", std::process::id()),
                pane_id: pane_id.into(),
                agent: agent.into(),
                launch_target: None,
                terminal_id: None,
                state: JobState::Running,
                observed_state: None,
                report: None,
                created_at_ms: time,
                updated_at_ms: time,
            };
            let task = data.tasks.get_mut(task_id).ok_or("task not found")?;
            task.job_ids.push(id.clone());
            task.status = TaskStatus::Running;
            task.agent = Some(agent.into());
            task.pane_id = Some(pane_id.into());
            task.review = None;
            task.results.clear();
            task.tests.clear();
            task.updated_at_ms = time;
            let description = task_description(task);
            data.jobs.insert(id.clone(), job.clone());
            data.record(
                "job.started",
                Some(task_id.into()),
                Some(id),
                format!("{description}\nDispatched to {agent} in {pane_id}"),
            )?;
            Ok(job)
        })
    }
    /// Bind physical terminal identity before submission. Pane locations can change.
    pub fn bind_job_runtime(
        &mut self,
        job_id: &str,
        target: &str,
        terminal_id: &str,
    ) -> Result<Job, String> {
        nonempty(target, "launch target", 240)?;
        nonempty(terminal_id, "terminal id", 240)?;
        self.transaction(|data| {
            if data.jobs.values().any(|j| {
                j.id != job_id && j.state.active() && j.terminal_id.as_deref() == Some(terminal_id)
            }) {
                return Err("terminal already has an active job".into());
            }
            let job = data.jobs.get_mut(job_id).ok_or("job not found")?;
            if !job.state.active() {
                return Err("cannot bind a terminal job".into());
            }
            if job
                .launch_target
                .as_deref()
                .is_some_and(|value| value != target)
                || job
                    .terminal_id
                    .as_deref()
                    .is_some_and(|value| value != terminal_id)
            {
                return Err("job is already bound to a different runtime".into());
            }
            job.launch_target = Some(target.into());
            job.terminal_id = Some(terminal_id.into());
            Ok(job.clone())
        })
    }
    pub fn report_job(
        &mut self,
        job_id: &str,
        token: &str,
        report: JobReport,
    ) -> Result<Job, String> {
        if !matches!(
            report.state,
            JobState::Succeeded | JobState::Failed | JobState::Blocked
        ) {
            return Err("report state must be succeeded, failed, or blocked".into());
        }
        if report.results.len() > 128 * 1024 || report.tests.len() > 64 * 1024 {
            return Err("job report exceeds limits".into());
        }
        if report.state == JobState::Succeeded && report.results.trim().is_empty() {
            return Err("successful jobs require a result summary".into());
        }
        self.transaction(|data| {
            let current = data.jobs.get(job_id).ok_or("job not found")?;
            if current.state == JobState::Interrupted {
                let latest = data.tasks.get(&current.task_id).and_then(|t| t.job_ids.last());
                if latest.map(String::as_str) != Some(job_id) {
                    return Err("interrupted job is stale because a newer attempt exists".into());
                }
                if report.state == JobState::Blocked && data.jobs.values().any(|j| j.state.active() && j.pane_id == current.pane_id) {
                    return Err("cannot reactivate an interrupted job while its pane has another active job".into());
                }
            }
            let job = data.jobs.get_mut(job_id).ok_or("job not found")?;
            if job.report_token != token {
                return Err("job report token does not match".into());
            }
            if job.state == report.state && job.report.as_ref() == Some(&report) {
                return Ok(job.clone());
            }
            if !job.state.active() && job.state != JobState::Interrupted {
                return Err(
                    "job is terminal; create a new job instead of changing its result".into(),
                );
            }
            job.state = report.state;
            job.report = Some(report.clone());
            job.updated_at_ms = now_ms();
            let job = job.clone();
            let task = data.tasks.get_mut(&job.task_id).ok_or("task not found")?;
            task.results = report.results.clone();
            task.tests = report.tests.clone();
            task.updated_at_ms = job.updated_at_ms;
            task.status = match report.state {
                JobState::Succeeded => TaskStatus::Review,
                _ => TaskStatus::Blocked,
            };
            let title = task.title.clone();
            data.record(
                "job.reported",
                Some(job.task_id.clone()),
                Some(job.id.clone()),
                format!(
                    "{title}: {:?}: {}\nTests: {}",
                    report.state, report.results, report.tests
                ),
            )?;
            Ok(job)
        })
    }
    /// A write acknowledgement was lost, or the runtime disappeared. Preserve uncertainty.
    pub fn interrupt_job(&mut self, job_id: &str, reason: &str) -> Result<Job, String> {
        if reason.len() > 64 * 1024 {
            return Err("interruption reason exceeds limits".into());
        }
        self.transaction(|data| {
            let job = data.jobs.get_mut(job_id).ok_or("job not found")?;
            if job.state == JobState::Interrupted {
                return Ok(job.clone());
            }
            if !job.state.active() {
                return Err("job is already terminal".into());
            }
            job.state = JobState::Interrupted;
            job.updated_at_ms = now_ms();
            let job = job.clone();
            let task = data.tasks.get_mut(&job.task_id).ok_or("task not found")?;
            task.status = TaskStatus::Blocked;
            task.updated_at_ms = job.updated_at_ms;
            data.record(
                "job.interrupted",
                Some(job.task_id.clone()),
                Some(job.id.clone()),
                reason.into(),
            )?;
            Ok(job)
        })
    }
    /// Move historical and active job references with a physical terminal. This is
    /// presentation provenance only: it never changes lifecycle or completion state.
    pub fn relocate_terminal(&mut self, terminal_id: &str, pane_id: &str) -> Result<(), String> {
        nonempty(terminal_id, "terminal id", 240)?;
        nonempty(pane_id, "pane id", 240)?;
        if !self
            .data
            .jobs
            .values()
            .any(|job| job.terminal_id.as_deref() == Some(terminal_id) && job.pane_id != pane_id)
        {
            return Ok(());
        }
        self.transaction(|data| {
            let ids: Vec<_> = data
                .jobs
                .values()
                .filter(|job| {
                    job.terminal_id.as_deref() == Some(terminal_id) && job.pane_id != pane_id
                })
                .map(|job| job.id.clone())
                .collect();
            for id in ids {
                let job = data.jobs.get_mut(&id).ok_or("job not found")?;
                let previous_pane = job.pane_id.clone();
                job.pane_id = pane_id.into();
                let task_id = job.task_id.clone();
                if let Some(task) = data.tasks.get_mut(&task_id) {
                    if task.job_ids.last() == Some(&id)
                        && task.pane_id.as_deref() == Some(&previous_pane)
                    {
                        task.pane_id = Some(pane_id.into());
                    }
                }
                data.record(
                    "job.location_changed",
                    Some(task_id),
                    Some(id),
                    format!("Job terminal moved from {previous_pane} to {pane_id}"),
                )?;
            }
            Ok(())
        })
    }
    /// Rebind a tracked job when the physical terminal moves between pane locations.
    pub fn observe_terminal_state(
        &mut self,
        terminal_id: &str,
        pane_id: &str,
        status: &str,
    ) -> Result<(), String> {
        nonempty(terminal_id, "terminal id", 240)?;
        nonempty(pane_id, "pane id", 240)?;
        let Some(job) = self.data.jobs.values().find(|j| {
            j.state.active()
                && (j.terminal_id.as_deref() == Some(terminal_id)
                    || (j.terminal_id.is_none() && j.pane_id == pane_id))
        }) else {
            return Ok(());
        };
        let job_id = job.id.clone();
        if job.pane_id != pane_id || job.terminal_id.is_none() {
            self.transaction(|data| {
                let job = data.jobs.get_mut(&job_id).ok_or("job not found")?;
                job.pane_id = pane_id.into();
                job.terminal_id = Some(terminal_id.into());
                let task_id = job.task_id.clone();
                if let Some(task) = data.tasks.get_mut(&task_id) {
                    task.pane_id = Some(pane_id.into());
                }
                data.record(
                    "job.location_changed",
                    Some(task_id),
                    Some(job_id),
                    format!("Job terminal now occupies {pane_id}"),
                )?;
                Ok(())
            })?;
        }
        self.observe_agent_state(pane_id, status)
    }
    /// Lifecycle changes are useful attention signals, never proof of task success.
    pub fn observe_agent_state(&mut self, pane_id: &str, status: &str) -> Result<(), String> {
        nonempty(status, "observed status", 64)?;
        let Some(job) = self
            .data
            .jobs
            .values()
            .find(|j| j.pane_id == pane_id && j.state.active())
        else {
            return Ok(());
        };
        if job.observed_state.as_deref() == Some(status) {
            return Ok(());
        }
        let job_id = job.id.clone();
        if matches!(status, "exited" | "missing" | "replaced") {
            self.interrupt_job(
                &job_id,
                &format!("Agent {status}; inspect the terminal before retrying"),
            )?;
            return Ok(());
        }
        self.transaction(|data| {
            let job = data.jobs.get_mut(&job_id).ok_or("job not found")?;
            job.observed_state = Some(status.into());
            let task_id = job.task_id.clone();
            let task = data.tasks.get_mut(&task_id).ok_or("task not found")?;
            if status == "blocked" {
                task.status = TaskStatus::Blocked;
            }
            if status == "working" && job.state == JobState::Running {
                task.status = TaskStatus::Running;
            }
            data.record(
                "agent.state_changed",
                Some(task_id),
                Some(job_id),
                format!("{pane_id}: {status}; job completion requires an explicit report"),
            )?;
            Ok(())
        })
    }
    pub fn cancel_job(&mut self, job_id: &str) -> Result<Job, String> {
        self.transaction(|data| {
            let job = data.jobs.get_mut(job_id).ok_or("job not found")?;
            if job.state == JobState::Cancelled {
                return Ok(job.clone());
            }
            if !job.state.active() {
                return Err("job is already terminal".into());
            }
            job.state = JobState::Cancelled;
            job.updated_at_ms = now_ms();
            let job = job.clone();
            let task = data.tasks.get_mut(&job.task_id).ok_or("task not found")?;
            task.status = TaskStatus::Blocked;
            task.updated_at_ms = job.updated_at_ms;
            data.record(
                "job.cancelled",
                Some(job.task_id.clone()),
                Some(job.id.clone()),
                "Tracking cancelled; terminal process is unchanged. Inspect it before retrying."
                    .into(),
            )?;
            Ok(job)
        })
    }
    pub fn review_task(
        &mut self,
        id: &str,
        decision: ReviewDecision,
        note: &str,
    ) -> Result<Task, String> {
        if note.len() > 64 * 1024 {
            return Err("review note exceeds limits".into());
        }
        self.transaction(|data| {
            let task = data.tasks.get_mut(id).ok_or("task not found")?;
            if task
                .review
                .as_ref()
                .is_some_and(|r| r.decision == decision && r.note == note)
            {
                return Ok(task.clone());
            }
            if task.status != TaskStatus::Review {
                return Err("only tasks awaiting review can be reviewed".into());
            }
            task.status = match decision {
                ReviewDecision::Accept => TaskStatus::Accepted,
                ReviewDecision::RequestChanges => TaskStatus::ChangesRequested,
            };
            task.updated_at_ms = now_ms();
            task.review = Some(Review {
                decision,
                note: note.into(),
                at_ms: task.updated_at_ms,
            });
            let task = task.clone();
            data.record(
                "task.reviewed",
                Some(id.into()),
                None,
                format!("{}: {:?}: {note}", task.title, decision),
            )?;
            Ok(task)
        })
    }
    pub fn save_template(
        &mut self,
        template: WorkflowTemplate,
    ) -> Result<WorkflowTemplate, String> {
        validate_template(&template)?;
        self.transaction(|data| {
            data.templates.insert(template.id.clone(), template.clone());
            data.record(
                "workflow.template_saved",
                None,
                None,
                format!("{} ({})", template.title, template.id),
            )?;
            Ok(template)
        })
    }
    pub fn start_workflow(
        &mut self,
        template_id: &str,
        brief: &str,
        cwd: Option<String>,
    ) -> Result<WorkflowRun, String> {
        nonempty(brief, "workflow brief", 128 * 1024)?;
        if let Some(cwd) = &cwd {
            nonempty(cwd, "cwd", 8192)?;
            if !Path::new(cwd).is_absolute() {
                return Err("workflow working directory must be absolute".into());
            }
        }
        self.transaction(|data| {
            let template = data
                .templates
                .get(template_id)
                .cloned()
                .ok_or("workflow template not found")?;
            let run = WorkflowRun {
                id: data.id("workflow")?,
                template,
                brief: brief.into(),
                cwd,
                tasks: BTreeMap::new(),
                created_at_ms: now_ms(),
            };
            data.record(
                "workflow.started",
                None,
                None,
                format!("{}: {}", run.template.title, run.brief),
            )?;
            data.workflows.insert(run.id.clone(), run.clone());
            Ok(run)
        })
    }
    pub fn workflow_run(&self, id: &str) -> Option<&WorkflowRun> {
        self.data.workflows.get(id)
    }
    pub fn ready_steps(&self, run_id: &str) -> Result<Vec<WorkflowStep>, String> {
        let run = self
            .data
            .workflows
            .get(run_id)
            .ok_or("workflow run not found")?;
        Ok(run
            .template
            .steps
            .iter()
            .filter(|step| {
                !run.tasks.contains_key(&step.id) && dependencies_accepted(&self.data, run, step)
            })
            .cloned()
            .collect())
    }
    pub fn start_step(&mut self, run_id: &str, step_id: &str) -> Result<Task, String> {
        self.transaction(|data| {
            let run = data
                .workflows
                .get(run_id)
                .cloned()
                .ok_or("workflow run not found")?;
            if let Some(task_id) = run.tasks.get(step_id) {
                return data
                    .tasks
                    .get(task_id)
                    .cloned()
                    .ok_or_else(|| "workflow task missing".into());
            }
            let step = run
                .template
                .steps
                .iter()
                .find(|s| s.id == step_id)
                .ok_or("workflow step not found")?;
            if !dependencies_accepted(data, &run, step) {
                return Err("step dependencies must be accepted before starting".into());
            }
            let mut brief = format!("{}\n\n{}", run.brief, step.prompt);
            for dependency in &step.depends_on {
                if let Some(task) = run.tasks.get(dependency).and_then(|id| data.tasks.get(id)) {
                    brief.push_str(&format!(
                        "\n\n{} results:\n{}\nTests:\n{}",
                        task.title, task.results, task.tests
                    ));
                }
            }
            let mut task = data.create(CreateTask {
                title: format!("{}: {}", run.template.title, step.title),
                brief,
                criteria: step.criteria.clone(),
                agent: step.agent.clone(),
                cwd: run.cwd.clone(),
                ..CreateTask::default()
            })?;
            task.workflow_run_id = Some(run_id.into());
            task.workflow_step_id = Some(step_id.into());
            data.tasks.insert(task.id.clone(), task.clone());
            data.workflows
                .get_mut(run_id)
                .ok_or("workflow run not found")?
                .tasks
                .insert(step_id.into(), task.id.clone());
            data.record(
                "workflow.step_started",
                Some(task.id.clone()),
                None,
                format!("{run_id}/{step_id}"),
            )?;
            Ok(task)
        })
    }
    /// Bounded pages preserve a durable cursor; callers can page without missing records.
    pub fn activity(&self, query: Option<&str>, after: u64, limit: usize) -> Vec<Activity> {
        let query = query.map(str::to_lowercase);
        self.data
            .activities
            .iter()
            .filter(|a| {
                a.seq > after
                    && query.as_ref().is_none_or(|q| {
                        format!(
                            "{} {} {} {}",
                            a.kind,
                            a.task_id.as_deref().unwrap_or(""),
                            a.job_id.as_deref().unwrap_or(""),
                            a.text
                        )
                        .to_lowercase()
                        .contains(q)
                    })
            })
            .take(limit.min(1000))
            .cloned()
            .collect()
    }
    /// Reading never acknowledges. `through` is an explicit, stable acknowledgement boundary.
    pub fn summary(&self, consumer: &str) -> ReturnSummary {
        let after = self.data.cursors.get(consumer).copied().unwrap_or(0);
        let activities = self.activity(None, after, 1000);
        let through = activities.last().map(|a| a.seq).unwrap_or(after);
        ReturnSummary {
            consumer: consumer.into(),
            after,
            through,
            activities,
            needs_review: self
                .data
                .tasks
                .values()
                .filter(|t| t.status == TaskStatus::Review)
                .cloned()
                .collect(),
            blocked: self
                .data
                .tasks
                .values()
                .filter(|t| matches!(t.status, TaskStatus::Blocked | TaskStatus::ChangesRequested))
                .cloned()
                .collect(),
            failed_jobs: self
                .data
                .jobs
                .values()
                .filter(|j| {
                    matches!(j.state, JobState::Failed | JobState::Interrupted)
                        && self.data.tasks.get(&j.task_id).is_some_and(|task| {
                            task.job_ids.last() == Some(&j.id)
                                && matches!(
                                    task.status,
                                    TaskStatus::Blocked | TaskStatus::ChangesRequested
                                )
                        })
                })
                .cloned()
                .collect(),
        }
    }
    pub fn acknowledge(&mut self, consumer: &str, through: u64) -> Result<(), String> {
        nonempty(consumer, "consumer", 240)?;
        self.transaction(|data| {
            if through >= data.next_seq {
                return Err("cannot acknowledge unseen future activity".into());
            }
            let cursor = data.cursors.entry(consumer.into()).or_default();
            *cursor = (*cursor).max(through);
            Ok(())
        })
    }
}

fn dependencies_accepted(data: &TaskData, run: &WorkflowRun, step: &WorkflowStep) -> bool {
    step.depends_on.iter().all(|id| {
        run.tasks
            .get(id)
            .and_then(|task| data.tasks.get(task))
            .is_some_and(|t| t.status == TaskStatus::Accepted)
    })
}

fn validate_template(template: &WorkflowTemplate) -> Result<(), String> {
    identifier(&template.id, "template id")?;
    nonempty(&template.title, "template title", 120)?;
    if template.steps.is_empty() || template.steps.len() > 100 {
        return Err("workflow must contain 1 to 100 steps".into());
    }
    let mut ids = BTreeSet::new();
    for step in &template.steps {
        identifier(&step.id, "step id")?;
        nonempty(&step.title, "step title", 100)?;
        nonempty(&step.prompt, "step prompt", 64 * 1024)?;
        if !ids.insert(step.id.as_str()) {
            return Err("duplicate workflow step id".into());
        }
        if step.criteria.len() > 100 || step.criteria.iter().any(|s| s.len() > 8192) {
            return Err("step criteria exceeds limits".into());
        }
    }
    let mut reached = BTreeSet::new();
    loop {
        let previous = reached.len();
        for step in &template.steps {
            if step.depends_on.iter().all(|d| reached.contains(d.as_str())) {
                reached.insert(step.id.as_str());
            }
        }
        if reached.len() == ids.len() {
            return Ok(());
        }
        if reached.len() == previous {
            return Err("workflow has a cycle or an unknown dependency".into());
        }
    }
}

fn validate_data(data: &TaskData) -> Result<(), String> {
    if data.version != 1 {
        return Err(format!(
            "unsupported task store version {}; original file preserved",
            data.version
        ));
    }
    if data.next_id == 0
        || data.next_seq == 0
        || data.activities.windows(2).any(|v| v[0].seq >= v[1].seq)
        || data
            .activities
            .last()
            .is_some_and(|a| a.seq >= data.next_seq)
    {
        return Err("invalid task store counters or activity; original file preserved".into());
    }
    for template in data.templates.values() {
        validate_template(template)?;
    }
    for (id, task) in &data.tasks {
        if id != &task.id {
            return Err("task store identity mismatch".into());
        }
        let mut unique = BTreeSet::new();
        for job_id in &task.job_ids {
            if !unique.insert(job_id) || data.jobs.get(job_id).is_none_or(|job| job.task_id != *id)
            {
                return Err("task store job history mismatch".into());
            }
        }
    }
    // IDs share one monotonic counter across entity kinds. Reject damaged counters
    // before creating a new entity could replace a previously saved record.
    for id in data
        .tasks
        .keys()
        .chain(data.jobs.keys())
        .chain(data.workflows.keys())
    {
        if id
            .rsplit_once('-')
            .and_then(|(_, n)| n.parse::<u64>().ok())
            .is_none_or(|n| n >= data.next_id)
        {
            return Err("task store ID counter is behind saved records".into());
        }
    }
    if data.cursors.values().any(|value| *value >= data.next_seq) {
        return Err("task store cursor exceeds saved activity".into());
    }
    let mut active_panes = BTreeSet::new();
    let mut active_tasks = BTreeSet::new();
    let mut active_terminals = BTreeSet::new();
    for (id, job) in &data.jobs {
        if id != &job.id
            || data
                .tasks
                .get(&job.task_id)
                .is_none_or(|task| !task.job_ids.contains(id))
        {
            return Err("task store job reference mismatch".into());
        }
        if job.state.active()
            && (!active_panes.insert(&job.pane_id)
                || !active_tasks.insert(&job.task_id)
                || job
                    .terminal_id
                    .as_ref()
                    .is_some_and(|id| !active_terminals.insert(id)))
        {
            return Err("task store contains duplicate active jobs".into());
        }
    }
    for (run_id, run) in &data.workflows {
        if run_id != &run.id {
            return Err("workflow identity mismatch".into());
        }
        validate_template(&run.template)?;
        for (step_id, task_id) in &run.tasks {
            if !run.template.steps.iter().any(|step| &step.id == step_id)
                || data.tasks.get(task_id).is_none_or(|task| {
                    task.workflow_run_id.as_ref() != Some(run_id)
                        || task.workflow_step_id.as_ref() != Some(step_id)
                })
            {
                return Err("workflow references missing or mismatched task".into());
            }
        }
    }
    Ok(())
}

fn save(path: &Path, data: &TaskData) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create task store directory: {e}"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temporary = path.with_extension(format!("json.{}.{nonce}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(data).map_err(|e| e.to_string())?;
    let result = (|| -> std::io::Result<()> {
        let mut file = crate::platform::create_private_state_file(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        crate::platform::replace_file(&temporary, path)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("cannot save task store: {error}"));
    }
    // The rename has committed. A directory sync failure cannot be rolled back safely;
    // publish the committed in-memory value and log the reduced crash durability.
    if let Err(error) = crate::platform::sync_parent_directory(parent) {
        tracing::warn!("task store parent sync failed: {error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
