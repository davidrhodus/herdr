//! Optional task/job API. These types travel as JSON, outside frozen terminal codecs.
pub use crate::tasks::{CreateTask, JobReport, ReviewDecision, TaskPatch, WorkflowTemplate};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskTarget {
    pub task_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskUpdateParams {
    pub task_id: String,
    pub patch: TaskPatch,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskReviewParams {
    pub task_id: String,
    pub decision: ReviewDecision,
    #[serde(default)]
    pub note: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskActivityParams {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub after: u64,
    #[serde(default = "default_limit")]
    pub limit: usize,
}
fn default_limit() -> usize {
    100
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskSummaryParams {
    pub consumer: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskAcknowledgeParams {
    pub consumer: String,
    pub through: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobStartParams {
    pub task_id: String,
    pub request_key: String,
    pub target: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobTarget {
    pub job_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobReportParams {
    pub job_id: String,
    pub token: String,
    pub report: JobReport,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowStartParams {
    pub template_id: String,
    pub brief: String,
    #[serde(default)]
    pub cwd: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowTarget {
    pub run_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkflowStepStartParams {
    pub run_id: String,
    pub step_id: String,
}
