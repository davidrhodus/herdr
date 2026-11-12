//! Task commands use the same server and machine routing as pane commands.
//! Job waits observe a durable job identity, never a terminal's idle status.

use std::time::{Duration, Instant};

use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{json, Value};

use super::projection::{self, ParseError};
use crate::api::schema::{Method, Request};

#[derive(Debug, PartialEq, Eq)]
struct JobWait {
    timeout_ms: Option<u64>,
    until: Vec<String>,
}

fn positional(name: &'static str) -> Arg {
    Arg::new(name).required(true)
}

fn option(name: &'static str) -> Arg {
    Arg::new(name).long(name).num_args(1)
}

fn file_option() -> Arg {
    option("file").help("Read the structured JSON payload from this local file")
}

pub(crate) fn command(group: &str) -> Command {
    match group {
        "task" => Command::new("task")
            .about("Track requests, results, review, and durable activity")
            .subcommand_required(true)
            .subcommand(Command::new("list").about("List persistent task cards"))
            .subcommand(Command::new("get").about("Read a task card").arg(positional("task-id")))
            .subcommand(
                Command::new("diff")
                    .about("Read the task worktree diff")
                    .arg(positional("task-id")),
            )
            .subcommand(
                Command::new("create")
                    .about("Create a persistent task")
                    .arg(
                        Arg::new("title")
                            .required_unless_present("file")
                            .conflicts_with("file"),
                    )
                    .arg(file_option())
                    .arg(option("brief").conflicts_with("file"))
                    .arg(
                        option("criterion")
                            .action(ArgAction::Append)
                            .conflicts_with("file"),
                    )
                    .arg(option("cwd").conflicts_with("file"))
                    .arg(option("branch").conflicts_with("file"))
                    .arg(option("agent").conflicts_with("file"))
                    .arg(option("pane").conflicts_with("file")),
            )
            .subcommand(
                Command::new("update")
                    .about("Apply a structured task patch")
                    .arg(positional("task-id"))
                    .arg(file_option().required(true)),
            )
            .subcommand(
                Command::new("review")
                    .about("Approve work or request changes")
                    .arg(positional("task-id"))
                    .arg(positional("decision").value_parser(["accept", "request_changes"]))
                    .arg(option("note").default_value("")),
            )
            .subcommand(
                Command::new("activity")
                    .about("Search durable task and job activity")
                    .arg(option("query"))
                    .arg(
                        option("after")
                            .value_parser(clap::value_parser!(u64))
                            .default_value("0"),
                    )
                    .arg(
                        option("limit")
                            .value_parser(clap::value_parser!(u64).range(1..=1000))
                            .default_value("100"),
                    ),
            )
            .subcommand(
                Command::new("summary")
                    .about("Show changes since this consumer last acknowledged them")
                    .arg(option("consumer").default_value("cli")),
            )
            .subcommand(
                Command::new("acknowledge")
                    .about("Advance a consumer's summary cursor explicitly")
                    .arg(option("consumer").default_value("cli"))
                    .arg(
                        option("through")
                            .required(true)
                            .value_parser(clap::value_parser!(u64)),
                    ),
            ),
        "job" => Command::new("job")
            .about("Submit, observe, and explicitly report individual agent jobs")
            .subcommand_required(true)
            .subcommand(
                Command::new("start")
                    .about("Submit a task to an idle agent with an idempotency key")
                    .arg(positional("task-id"))
                    .arg(
                        option("target")
                            .required(true)
                            .help("Live agent name or pane ID"),
                    )
                    .arg(
                        option("request-key")
                            .required(true)
                            .help("Unique submission key; reuse only to recover the same job"),
                    ),
            )
            .subcommand(Command::new("get").about("Read an exact job record").arg(positional("job-id")))
            .subcommand(
                Command::new("wait")
                    .about("Wait for this exact job's explicit report; never resubmits")
                    .arg(positional("job-id"))
                    .arg(
                        option("timeout")
                            .value_parser(clap::value_parser!(u64).range(1..))
                            .help("Maximum wait in milliseconds"),
                    )
                    .arg(
                        option("until")
                            .action(ArgAction::Append)
                            .value_parser([
                                "succeeded",
                                "failed",
                                "cancelled",
                                "interrupted",
                                "blocked",
                            ])
                            .help("Accept these exact job states; repeat to accept several"),
                    ),
            )
            .subcommand(
                Command::new("report")
                    .about("Report the exact job using its completion token")
                    .arg(positional("job-id"))
                    .arg(option("token").required(true))
                    .arg(
                        file_option()
                            .required_unless_present("status")
                            .conflicts_with("status")
                            .help("JSON JobReport with state, results, and tests"),
                    )
                    .arg(
                        option("status")
                            .required_unless_present("file")
                            .value_parser(["succeeded", "failed", "blocked"]),
                    )
                    .arg(option("result").conflicts_with("file"))
                    .arg(option("tests").conflicts_with("file")),
            )
            .subcommand(
                Command::new("cancel")
                    .about("Cancel tracking; does not kill the agent process")
                    .arg(positional("job-id")),
            ),
        _ => Command::new("workflow")
            .about("Save and run reusable dependency-ordered task workflows")
            .subcommand_required(true)
            .subcommand(Command::new("list").about("List saved workflow templates"))
            .subcommand(
                Command::new("save")
                    .about("Save a structured workflow template")
                    .arg(file_option().required(true)),
            )
            .subcommand(
                Command::new("start")
                    .about("Create a persistent workflow run")
                    .arg(positional("template-id"))
                    .arg(option("brief").required(true))
                    .arg(option("cwd")),
            )
            .subcommand(Command::new("get").about("Read a workflow run and its eligible steps").arg(positional("run-id")))
            .subcommand(
                Command::new("setup")
                    .about("Create an isolated worktree, start an agent, and launch the first workflow step")
                    .arg(positional("template-id"))
                    .arg(option("brief").required(true))
                    .arg(option("workspace").required(true))
                    .arg(option("branch").required(true))
                    .arg(option("kind").required(true))
                    .arg(option("timeout").value_parser(clap::value_parser!(u64).range(3001..=300000)).default_value("30000")),
            )
            .subcommand(
                Command::new("launch")
                    .about("Create an eligible step task and submit it to an idle agent")
                    .arg(positional("run-id"))
                    .arg(positional("step-id"))
                    .arg(option("target").required(true))
                    .arg(option("request-key").required(true)),
            )
            .subcommand(
                Command::new("step-start")
                    .about("Create the step's task after its dependencies are approved")
                    .arg(positional("run-id"))
                    .arg(positional("step-id")),
            ),
    }
}

fn text(matches: &ArgMatches, name: &str) -> String {
    matches.get_one::<String>(name).cloned().unwrap_or_default()
}

fn json_file(path: &str) -> Result<Value, ParseError> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > 512 * 1024 {
        return Err(ParseError::Usage(
            "JSON payload must be at most 512 KiB".into(),
        ));
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|error| ParseError::Usage(format!("invalid JSON payload: {error}")))?;
    if !value.is_object() {
        return Err(ParseError::Usage("JSON payload must be an object".into()));
    }
    Ok(value)
}

fn request(group: &str, args: &[String]) -> Result<(Request, Option<JobWait>), ParseError> {
    projection::parse(
        command(group).bin_name(format!("herdr {group}")),
        args,
        |matches| {
            let (action, m) = matches
                .subcommand()
                .ok_or_else(|| ParseError::Usage("missing command".into()))?;
            let mut wait = None;
            let (method, params) = match (group, action) {
                ("task", "list") => ("task.list", json!({})),
                ("task", "get") => ("task.get", json!({"task_id":text(m,"task-id")})),
                ("task", "diff") => ("task.diff", json!({"task_id":text(m,"task-id")})),
                ("task", "create") => {
                    let params = if let Some(path) = m.get_one::<String>("file") {
                        json_file(path)?
                    } else {
                        let criteria: Vec<String> = m
                            .get_many::<String>("criterion")
                            .into_iter()
                            .flatten()
                            .cloned()
                            .collect();
                        json!({"title":text(m,"title"),"brief":text(m,"brief"),"criteria":criteria,
                        "cwd":m.get_one::<String>("cwd"),"branch":m.get_one::<String>("branch"),
                        "agent":m.get_one::<String>("agent"),"pane_id":m.get_one::<String>("pane")})
                    };
                    ("task.create", params)
                }
                ("task", "update") => (
                    "task.update",
                    json!({"task_id":text(m,"task-id"),"patch":json_file(&text(m,"file"))?}),
                ),
                ("task", "review") => (
                    "task.review",
                    json!({"task_id":text(m,"task-id"),"decision":text(m,"decision"),"note":text(m,"note")}),
                ),
                ("task", "activity") => (
                    "task.activity",
                    json!({"query":m.get_one::<String>("query"),"after":m.get_one::<u64>("after"),"limit":m.get_one::<u64>("limit")}),
                ),
                ("task", "summary") => ("task.summary", json!({"consumer":text(m,"consumer")})),
                ("task", "acknowledge") => (
                    "task.acknowledge",
                    json!({"consumer":text(m,"consumer"),"through":m.get_one::<u64>("through")}),
                ),
                ("job", "start") => (
                    "job.start",
                    json!({"task_id":text(m,"task-id"),"target":text(m,"target"),"request_key":text(m,"request-key")}),
                ),
                ("job", "get" | "wait") => {
                    if action == "wait" {
                        wait = Some(JobWait {
                            timeout_ms: m.get_one::<u64>("timeout").copied(),
                            until: m
                                .get_many::<String>("until")
                                .map(|values| values.cloned().collect())
                                .unwrap_or_else(|| {
                                    vec![
                                        "succeeded".into(),
                                        "failed".into(),
                                        "cancelled".into(),
                                        "interrupted".into(),
                                    ]
                                }),
                        });
                    }
                    ("job.get", json!({"job_id":text(m,"job-id")}))
                }
                ("job", "cancel") => ("job.cancel", json!({"job_id":text(m,"job-id")})),
                ("job", "report") => {
                    let report = if let Some(path) = m.get_one::<String>("file") {
                        json_file(path)?
                    } else {
                        json!({"state":text(m,"status"),"results":text(m,"result"),"tests":text(m,"tests")})
                    };
                    (
                        "job.report",
                        json!({"job_id":text(m,"job-id"),"token":text(m,"token"),"report":report}),
                    )
                }
                ("workflow", "list") => ("workflow.list", json!({})),
                ("workflow", "save") => ("workflow.save", json_file(&text(m, "file"))?),
                ("workflow", "start") => (
                    "workflow.start",
                    json!({"template_id":text(m,"template-id"),"brief":text(m,"brief"),"cwd":m.get_one::<String>("cwd")}),
                ),
                ("workflow", "get") => ("workflow.get", json!({"run_id":text(m,"run-id")})),
                ("workflow", "step-start") => (
                    "workflow.step.start",
                    json!({"run_id":text(m,"run-id"),"step_id":text(m,"step-id")}),
                ),
                _ => return Err(ParseError::Usage("unsupported command".into())),
            };
            let method: Method = serde_json::from_value(json!({"method":method,"params":params}))
                .map_err(|error| {
                ParseError::Usage(format!("invalid {group} payload: {error}"))
            })?;
            Ok((
                Request {
                    id: format!("cli:{group}:{action}"),
                    method,
                },
                wait,
            ))
        },
    )
}

pub(super) fn run(group: &str, args: &[String]) -> std::io::Result<i32> {
    if group == "workflow" && args.first().is_some_and(|arg| arg == "setup") {
        return run_setup(args);
    }
    if group == "workflow" && args.first().is_some_and(|arg| arg == "launch") {
        return run_launch(args);
    }
    let (request, wait) = match request(group, args) {
        Ok(parsed) => parsed,
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Io(error)) => return Err(error),
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
    };
    let Some(wait) = wait else {
        return super::print_response(&super::send_request(&request)?);
    };
    wait_for_job(&request, &wait, super::send_request, std::thread::sleep)
}

#[derive(Debug)]
struct SetupOptions {
    template_id: String,
    brief: String,
    workspace: String,
    branch: String,
    kind: String,
    timeout_ms: u64,
}

fn setup_options(args: &[String]) -> Result<SetupOptions, ParseError> {
    projection::parse(
        command("workflow").bin_name("herdr workflow"),
        args,
        |matches| {
            let m = matches
                .subcommand_matches("setup")
                .ok_or_else(|| ParseError::Usage("missing workflow setup command".into()))?;
            let kind = text(m, "kind");
            let agent = crate::detect::parse_agent_label(&kind).ok_or_else(|| {
                ParseError::Usage(format!("unsupported interactive agent kind: {kind}"))
            })?;
            let options = SetupOptions {
                template_id: text(m, "template-id"),
                brief: text(m, "brief"),
                workspace: text(m, "workspace"),
                branch: text(m, "branch"),
                kind: crate::detect::agent_label(agent).into(),
                timeout_ms: m.get_one::<u64>("timeout").copied().unwrap_or(30000),
            };
            if [
                &options.template_id,
                &options.brief,
                &options.workspace,
                &options.branch,
            ]
            .iter()
            .any(|value| value.trim().is_empty() || value.contains('\0'))
            {
                return Err(ParseError::Usage(
                    "workflow setup values must contain visible text and no NUL bytes".into(),
                ));
            }
            Ok(options)
        },
    )
}

fn run_setup(args: &[String]) -> std::io::Result<i32> {
    let options = match setup_options(args) {
        Ok(options) => options,
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Io(error)) => return Err(error),
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
    };
    let response = setup_workflow(&options, super::send_request, std::thread::sleep);
    super::print_response(&response)
}

fn setup_error(stage: &str, cause: Value, resources: &Value) -> Value {
    json!({"id":"cli:workflow:setup","error":{
        "code":"workflow_setup_failed",
        "message":format!("Workflow setup stopped during {stage}; created resources were retained. Inspect resources before retrying."),
        "stage":stage,"cause":cause,"resources":resources,
    }})
}

fn setup_request(
    method: Method,
    stage: &str,
    resources: &Value,
    send: &mut impl FnMut(&Request) -> std::io::Result<Value>,
) -> Result<Value, Value> {
    match send(&Request {
        id: format!("cli:workflow:setup:{stage}"),
        method,
    }) {
        Ok(response) if response.get("error").is_none() => Ok(response),
        Ok(response) => Err(setup_error(stage, response["error"].clone(), resources)),
        Err(error) => Err(setup_error(
            stage,
            json!({"code":"transport_error","message":error.to_string(),"submission_may_have_been_accepted":true}),
            resources,
        )),
    }
}

fn setup_workflow(
    options: &SetupOptions,
    mut send: impl FnMut(&Request) -> std::io::Result<Value>,
    mut sleep: impl FnMut(Duration),
) -> Value {
    use crate::api::schema::*;
    let mut resources = json!({});
    let result = (|| -> Result<Value, Value> {
        let listed = setup_request(
            Method::WorkflowList(EmptyParams {}),
            "template",
            &resources,
            &mut send,
        )?;
        let template = listed["result"]["templates"]
            .as_array()
            .and_then(|templates| {
                templates
                    .iter()
                    .find(|template| template["id"].as_str() == Some(options.template_id.as_str()))
            })
            .ok_or_else(|| {
                setup_error(
                    "template",
                    json!({"message":"saved workflow template not found"}),
                    &resources,
                )
            })?;
        let template: crate::tasks::WorkflowTemplate = serde_json::from_value(template.clone())
            .map_err(|error| {
                setup_error("template", json!({"message":error.to_string()}), &resources)
            })?;
        template
            .steps
            .iter()
            .find(|step| step.depends_on.is_empty())
            .ok_or_else(|| {
                setup_error(
                    "template",
                    json!({"message":"template has no initial step"}),
                    &resources,
                )
            })?;
        let created = setup_request(
            Method::WorktreeCreate(WorktreeCreateParams {
                workspace_id: Some(options.workspace.clone()),
                branch: Some(options.branch.clone()),
                label: Some(template.title.clone()),
                ..Default::default()
            }),
            "worktree",
            &resources,
            &mut send,
        )?;
        // Preserve the whole creation receipt before validating any optional data.
        resources["worktree"] = created["result"]["worktree"].clone();
        resources["workspace"] = created["result"]["workspace"].clone();
        resources["root_pane"] = created["result"]["root_pane"].clone();
        let cwd = resources["worktree"]["path"]
            .as_str()
            .ok_or_else(|| {
                setup_error(
                    "worktree",
                    json!({"message":"creation response omitted worktree path"}),
                    &resources,
                )
            })?
            .to_owned();
        let pane = resources["root_pane"]["pane_id"]
            .as_str()
            .ok_or_else(|| {
                setup_error(
                    "worktree",
                    json!({"message":"creation response omitted root pane ID"}),
                    &resources,
                )
            })?
            .to_owned();
        let run = setup_request(
            Method::WorkflowStart(WorkflowStartParams {
                template_id: options.template_id.clone(),
                brief: options.brief.clone(),
                cwd: Some(cwd.clone()),
            }),
            "run",
            &resources,
            &mut send,
        )?;
        resources["run"] = run["result"]["run"].clone();
        let first: crate::tasks::WorkflowStep = run["result"]["ready_steps"]
            .as_array()
            .and_then(|steps| steps.first())
            .cloned()
            .ok_or_else(|| {
                setup_error(
                    "run",
                    json!({"message":"workflow has no eligible initial step"}),
                    &resources,
                )
            })
            .and_then(|value| {
                serde_json::from_value(value).map_err(|error| {
                    setup_error("run", json!({"message":error.to_string()}), &resources)
                })
            })?;
        let run_id = resources["run"]["id"]
            .as_str()
            .ok_or_else(|| {
                setup_error(
                    "run",
                    json!({"message":"workflow response omitted run ID"}),
                    &resources,
                )
            })?
            .to_owned();
        let name = format!("wf-{run_id}");
        resources["agent_name"] = json!(name);
        let started = setup_request(
            Method::AgentStart(AgentStartParams {
                name: name.clone(),
                kind: options.kind.clone(),
                pane_id: pane.clone(),
                args: Vec::new(),
                timeout_ms: Some(options.timeout_ms),
            }),
            "agent_start",
            &resources,
            &mut send,
        )?;
        resources["agent"] = started["result"]["agent"].clone();
        let terminal_id = resources["agent"]["terminal_id"]
            .as_str()
            .ok_or_else(|| {
                setup_error(
                    "agent_start",
                    json!({"message":"agent response omitted terminal ID"}),
                    &resources,
                )
            })?
            .to_owned();
        let deadline = Instant::now() + Duration::from_millis(options.timeout_ms);
        loop {
            let observed = setup_request(
                Method::AgentGet(AgentTarget {
                    target: name.clone(),
                }),
                "agent_ready",
                &resources,
                &mut send,
            )?;
            let agent = &observed["result"]["agent"];
            if agent["terminal_id"].as_str() != Some(terminal_id.as_str())
                || agent["name"].as_str() != Some(name.as_str())
            {
                return Err(setup_error(
                    "agent_ready",
                    json!({"message":"named agent no longer owns the created terminal"}),
                    &resources,
                ));
            }
            if let Some(actual) = agent["agent"].as_str() {
                if actual != options.kind {
                    return Err(setup_error(
                        "agent_ready",
                        json!({"message":"detected agent kind differs from requested kind"}),
                        &resources,
                    ));
                }
            }
            if agent["agent_status"].as_str() == Some("blocked") {
                return Err(setup_error(
                    "agent_ready",
                    json!({"message":"agent needs input during startup; inspect its pane"}),
                    &resources,
                ));
            }
            if matches!(agent["agent_status"].as_str(), Some("idle" | "done"))
                && agent["interactive_ready"].as_bool() == Some(true)
            {
                resources["agent"] = agent.clone();
                break;
            }
            if Instant::now() >= deadline {
                return Err(setup_error(
                    "agent_ready",
                    json!({"message":"timed out waiting for the agent to become interactive"}),
                    &resources,
                ));
            }
            sleep(
                Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        let step = setup_request(
            Method::WorkflowStepStart(WorkflowStepStartParams {
                run_id: run_id.clone(),
                step_id: first.id.clone(),
            }),
            "step",
            &resources,
            &mut send,
        )?;
        resources["task"] = step["result"]["task"].clone();
        let task_id = resources["task"]["id"]
            .as_str()
            .ok_or_else(|| {
                setup_error(
                    "step",
                    json!({"message":"step response omitted task ID"}),
                    &resources,
                )
            })?
            .to_owned();
        let updated = setup_request(
            Method::TaskUpdate(TaskUpdateParams {
                task_id: task_id.clone(),
                patch: TaskPatch {
                    cwd: Some(cwd),
                    branch: Some(options.branch.clone()),
                    pane_id: Some(pane),
                    agent: Some(name.clone()),
                    ..Default::default()
                },
            }),
            "task",
            &resources,
            &mut send,
        )?;
        resources["task"] = updated["result"]["task"].clone();
        let key = format!("setup-{run_id}-{}", first.id);
        resources["request_key"] = json!(key);
        let job = setup_request(
            Method::JobStart(JobStartParams {
                task_id,
                request_key: key,
                target: name,
            }),
            "job",
            &resources,
            &mut send,
        )?;
        resources["job"] = job["result"]["job"].clone();
        Ok(
            json!({"id":"cli:workflow:setup","result":{"type":"workflow_setup","resources":resources}}),
        )
    })();
    match result {
        Ok(response) | Err(response) => response,
    }
}

fn run_launch(args: &[String]) -> std::io::Result<i32> {
    let parsed = projection::parse(command("workflow"), args, |matches| {
        let m = matches
            .subcommand_matches("launch")
            .ok_or_else(|| ParseError::Usage("missing workflow launch command".into()))?;
        Ok((
            text(m, "run-id"),
            text(m, "step-id"),
            text(m, "target"),
            text(m, "request-key"),
        ))
    });
    let (run_id, step_id, target, key) = match parsed {
        Ok(parsed) => parsed,
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Io(error)) => return Err(error),
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
    };
    let response = launch_step(&run_id, &step_id, &target, &key, super::send_request)?;
    super::print_response(&response)
}

fn launch_step(
    run_id: &str,
    step_id: &str,
    target: &str,
    key: &str,
    mut send: impl FnMut(&Request) -> std::io::Result<Value>,
) -> std::io::Result<Value> {
    // The server makes step creation idempotent and checks dependency approval.
    // Reusing the submission key recovers the original job without replaying input.
    let request = Request {
        id: "cli:workflow:launch:step".into(),
        method: Method::WorkflowStepStart(crate::api::schema::WorkflowStepStartParams {
            run_id: run_id.into(),
            step_id: step_id.into(),
        }),
    };
    let response = send(&request)?;
    if response.get("error").is_some() {
        return Ok(response);
    }
    let task_id = response["result"]["task"]["id"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("workflow step response did not contain a task ID"))?;
    send(&Request {
        id: "cli:workflow:launch:job".into(),
        method: Method::JobStart(crate::api::schema::JobStartParams {
            task_id: task_id.into(),
            request_key: key.into(),
            target: target.into(),
        }),
    })
}

fn job_finished(response: &Value, until: &[String]) -> Result<bool, String> {
    let status = response["result"]["job"]["state"]
        .as_str()
        .ok_or_else(|| "job response did not contain a state".to_string())?;
    match status {
        "blocked" | "succeeded" | "failed" | "cancelled" | "interrupted" | "running" => {
            Ok(until.iter().any(|expected| expected == status))
        }
        _ => Err(format!("unsupported job state: {status}")),
    }
}

fn wait_for_job(
    request: &Request,
    wait: &JobWait,
    mut send: impl FnMut(&Request) -> std::io::Result<Value>,
    mut sleep: impl FnMut(Duration),
) -> std::io::Result<i32> {
    let started = Instant::now();
    loop {
        let response = send(request)?;
        if response.get("error").is_some() {
            return super::print_response(&response);
        }
        if let Method::JobGet(target) = &request.method {
            if response["result"]["job"]["id"].as_str() != Some(target.job_id.as_str()) {
                eprintln!(
                    "{}",
                    json!({"id":request.id,"error":{"code":"invalid_job_response","message":"The response does not identify the requested job"}})
                );
                return Ok(1);
            }
        }
        match job_finished(&response, &wait.until) {
            Ok(true) => return super::print_response(&response),
            Err(message) => {
                eprintln!(
                    "{}",
                    json!({"id":request.id,"error":{"code":"invalid_job_response","message":message}})
                );
                return Ok(1);
            }
            Ok(false) => {}
        }
        let interval = Duration::from_millis(200);
        if let Some(timeout_ms) = wait.timeout_ms {
            let timeout = Duration::from_millis(timeout_ms);
            if started.elapsed() >= timeout {
                eprintln!(
                    "{}",
                    json!({"id":request.id,"error":{"code":"timeout","message":"Timed out waiting for the job report; the job was not resubmitted or cancelled"}})
                );
                return Ok(1);
            }
            sleep(interval.min(timeout.saturating_sub(started.elapsed())));
        } else {
            sleep(interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(group: &str, args: &[&str]) -> Result<(Request, Option<JobWait>), ParseError> {
        request(
            group,
            &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn task_commands_validate_before_connecting() {
        for (group, args) in [
            ("task", vec!["review", "t1", "delete"]),
            ("job", vec!["start", "t1", "--target", "reviewer"]),
            ("job", vec!["wait", "j1", "--timeout", "0"]),
            ("task", vec!["activity", "--limit", "0"]),
            ("workflow", vec!["start", "implement-test-review"]),
        ] {
            assert!(parse(group, &args).is_err(), "{group} {args:?}");
        }
        for group in ["task", "job", "workflow"] {
            command(group).debug_assert();
        }
    }

    #[test]
    fn job_wait_never_uses_agent_idle_or_resubmits() {
        let (request, wait) = parse("job", &["wait", "j17", "--timeout", "1000"]).unwrap();
        assert_eq!(wait.as_ref().and_then(|w| w.timeout_ms), Some(1000));
        let mut calls = 0;
        assert_eq!(wait_for_job(&request,&wait.unwrap(),|request| {
            let serialized = serde_json::to_value(request).unwrap();
            assert_eq!(serialized["method"],"job.get");
            assert_eq!(serialized["params"]["job_id"],"j17");
            calls += 1;
            Ok(json!({"result":{"job":{"id":"j17","state":if calls == 1 {"running"} else {"succeeded"}}}}))
        }, |_| {}).unwrap(),0);
        assert_eq!(calls, 2);
        assert!(job_finished(
            &json!({"result":{"agent":{"agent_status":"idle"}}}),
            &["succeeded".into()]
        )
        .is_err());
    }

    #[test]
    fn job_wait_timeout_leaves_job_alone() {
        let (request, _) = parse("job", &["wait", "j17"]).unwrap();
        let mut calls = 0;
        assert_eq!(
            wait_for_job(
                &request,
                &JobWait {
                    timeout_ms: Some(0),
                    until: vec!["succeeded".into()]
                },
                |_| {
                    calls += 1;
                    Ok(json!({"result":{"job":{"id":"j17","state":"running"}}}))
                },
                |_| panic!("deadline already passed")
            )
            .unwrap(),
            1
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn task_activity_and_summary_have_explicit_cursors() {
        let (request, _) = parse(
            "task",
            &[
                "activity", "--query", "login", "--after", "7", "--limit", "25",
            ],
        )
        .unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(
            value["params"],
            json!({"query":"login","after":7,"limit":25})
        );
        let (request, _) = parse("task", &["summary", "--consumer", "david"]).unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap()["method"],
            "task.summary"
        );
    }

    #[test]
    fn report_flags_preserve_evidence_and_token() {
        let (request, _) = parse(
            "job",
            &[
                "report",
                "j9",
                "--token",
                "token9",
                "--status",
                "succeeded",
                "--result",
                "Fixed redirect",
                "--tests",
                "cargo test login: passed",
            ],
        )
        .unwrap();
        let request = serde_json::to_value(request).unwrap();
        assert_eq!(
            request["params"],
            json!({"job_id":"j9","token":"token9","report":{"state":"succeeded","results":"Fixed redirect","tests":"cargo test login: passed"}})
        );
    }

    #[test]
    fn blocked_is_only_accepted_when_requested() {
        let response = json!({"result":{"job":{"state":"blocked"}}});
        assert!(!job_finished(&response, &["succeeded".into(), "failed".into()]).unwrap());
        assert!(job_finished(&response, &["blocked".into()]).unwrap());
        let (_, wait) = parse(
            "job",
            &["wait", "j9", "--until", "blocked", "--until", "succeeded"],
        )
        .unwrap();
        assert_eq!(wait.unwrap().until, vec!["blocked", "succeeded"]);
    }

    #[test]
    fn job_wait_rejects_a_response_for_another_job() {
        let (request, wait) = parse("job", &["wait", "j17"]).unwrap();
        assert_eq!(
            wait_for_job(
                &request,
                &wait.unwrap(),
                |_| Ok(json!({"result":{"job":{"id":"j18","state":"succeeded"}}})),
                |_| panic!("must stop on a mismatched identity")
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn workflow_launch_stops_at_rejected_dependencies() {
        let mut calls = 0;
        let result = launch_step("run1", "test", "tester", "attempt1", |request| {
            calls += 1;
            assert!(matches!(request.method, Method::WorkflowStepStart(_)));
            Ok(json!({"error":{"code":"task_error","message":"dependency not accepted"}}))
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert!(result.get("error").is_some());
    }

    #[test]
    fn workflow_launch_submits_the_created_task_with_the_original_key() {
        let mut calls = 0;
        launch_step("run1", "implement", "coder", "attempt1", |request| {
            calls += 1;
            if calls == 1 {
                assert!(matches!(request.method, Method::WorkflowStepStart(_)));
                Ok(json!({"result":{"task":{"id":"t7"}}}))
            } else {
                let request = serde_json::to_value(request).unwrap();
                assert_eq!(request["method"], "job.start");
                assert_eq!(
                    request["params"],
                    json!({"task_id":"t7","target":"coder","request_key":"attempt1"})
                );
                Ok(json!({"result":{"job":{"id":"j8","state":"running"}}}))
            }
        })
        .unwrap();
        assert_eq!(calls, 2);
    }

    fn setup_test_options() -> SetupOptions {
        setup_options(
            &[
                "setup",
                "implement-test-review",
                "--brief",
                "Fix login; $(literal text)",
                "--workspace",
                "w1",
                "--branch",
                "fix/login",
                "--kind",
                "codex",
            ]
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn workflow_setup_validates_kind_and_all_options_before_connecting() {
        for arguments in [
            vec![
                "setup",
                "implement-test-review",
                "--brief",
                "change",
                "--workspace",
                "w1",
                "--branch",
                "fix/login",
            ],
            vec![
                "setup",
                "implement-test-review",
                "--brief",
                "change",
                "--workspace",
                "w1",
                "--branch",
                "fix/login",
                "--kind",
                "not-an-agent",
            ],
        ] {
            assert!(setup_options(
                &arguments
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>()
            )
            .is_err());
        }
    }

    #[test]
    fn workflow_setup_launches_first_step_in_an_isolated_worktree() {
        let options = setup_test_options();
        let template = crate::tasks::default_workflow();
        let mut methods = Vec::new();
        let response = setup_workflow(
            &options,
            |request| {
                let value = serde_json::to_value(request).unwrap();
                methods.push(value["method"].as_str().unwrap().to_owned());
                let result = match &request.method {
                    Method::WorkflowList(_) => json!({"templates":[template]}),
                    Method::WorktreeCreate(params) => {
                        assert_eq!(params.workspace_id.as_deref(), Some("w1"));
                        assert_eq!(params.branch.as_deref(), Some("fix/login"));
                        json!({"worktree":{"path":"/worktrees/login","branch":"fix/login"},"workspace":{"id":"w2"},"root_pane":{"pane_id":"w2:p1"}})
                    }
                    Method::WorkflowStart(params) => {
                        assert_eq!(params.cwd.as_deref(), Some("/worktrees/login"));
                        assert_eq!(params.brief, "Fix login; $(literal text)");
                        json!({"run":{"id":"run1"},"ready_steps":[template.steps[0]]})
                    }
                    Method::AgentStart(params) => {
                        assert_eq!(params.name, "wf-run1");
                        assert_eq!(params.pane_id, "w2:p1");
                        assert_eq!(params.kind, "codex");
                        json!({"agent":{"terminal_id":"terminal1"}})
                    }
                    Method::AgentGet(params) => {
                        assert_eq!(params.target, "wf-run1");
                        json!({"agent":{"terminal_id":"terminal1","name":"wf-run1","agent":"codex","agent_status":"idle","interactive_ready":true}})
                    }
                    Method::WorkflowStepStart(params) => {
                        assert_eq!(params.run_id, "run1");
                        assert_eq!(params.step_id, "implement");
                        json!({"task":{"id":"t1"}})
                    }
                    Method::TaskUpdate(params) => {
                        assert_eq!(params.patch.cwd.as_deref(), Some("/worktrees/login"));
                        assert_eq!(params.patch.branch.as_deref(), Some("fix/login"));
                        json!({"task":{"id":"t1","cwd":"/worktrees/login"}})
                    }
                    Method::JobStart(params) => {
                        assert_eq!(params.task_id, "t1");
                        assert_eq!(params.target, "wf-run1");
                        assert_eq!(params.request_key, "setup-run1-implement");
                        json!({"job":{"id":"j1","state":"running"}})
                    }
                    _ => panic!("unexpected operation {value}"),
                };
                Ok(json!({"result":result}))
            },
            |_| panic!("test agent is immediately ready"),
        );
        assert_eq!(response["result"]["resources"]["job"]["id"], "j1");
        assert_eq!(response["result"]["resources"]["agent_name"], "wf-run1");
        assert_eq!(
            methods,
            vec![
                "workflow.list",
                "worktree.create",
                "workflow.start",
                "agent.start",
                "agent.get",
                "workflow.step.start",
                "task.update",
                "job.start"
            ]
        );
    }

    #[test]
    fn workflow_setup_retains_resources_after_partial_failure() {
        let template = crate::tasks::default_workflow();
        let mut calls = 0;
        let response = setup_workflow(
            &setup_test_options(),
            |request| {
                calls += 1;
                match &request.method {
                    Method::WorkflowList(_) => Ok(json!({"result":{"templates":[template]}})),
                    Method::WorktreeCreate(_) => Ok(
                        json!({"result":{"worktree":{"path":"/worktrees/login"},"workspace":{"id":"w2"},"root_pane":{"pane_id":"w2:p1"}}}),
                    ),
                    Method::WorkflowStart(_) => Ok(
                        json!({"result":{"run":{"id":"run1"},"ready_steps":[template.steps[0]]}}),
                    ),
                    Method::AgentStart(_) => Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "connection lost",
                    )),
                    _ => panic!("no cleanup or retry after a partial failure"),
                }
            },
            |_| panic!("no agent to wait for"),
        );
        assert_eq!(calls, 4);
        assert_eq!(response["error"]["stage"], "agent_start");
        assert_eq!(response["error"]["resources"]["workspace"]["id"], "w2");
        assert_eq!(response["error"]["resources"]["run"]["id"], "run1");
        assert_eq!(
            response["error"]["cause"]["submission_may_have_been_accepted"],
            true
        );
    }
}
