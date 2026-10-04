//! One durable two-worker invocation. The host owns presentation; this module
//! owns worker lifetimes and accepts completions in native event order.
pub(crate) mod approvals;
mod completion;
pub(crate) mod questions;
pub(crate) mod state;
mod tool_calls;

use crate::{
    board::{Board, WaitCursor},
    protocol::{Event, HostCommand, StartRequest},
    workers::{RpcClient, text_input, verify_thread_response, worker_config},
    workspace::{self, PreparedWorkspace},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use state::{Journal, RunLock, atomic_json, now};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;

// Give the runtime a bounded opportunity to start acknowledged native shutdown
// at the work deadline. The watchdog still stops an unresponsive runtime, and
// its separate native-shutdown bound remains in force once handoff begins.
const WATCHDOG_HANDOFF_GRACE: Duration = Duration::from_secs(2);
const TIME_LIMIT_REACHED: &str = "The execution time limit was reached";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Worker {
    thread: String,
    turn: Option<String>,
    revision: u64,
    outcome: Option<Value>,
    checks: HashMap<String, Value>,
    #[serde(default)]
    command_revisions: HashMap<String, u64>,
    #[serde(default)]
    command_sequences: HashMap<String, u64>,
    #[serde(default)]
    revision_fences: Vec<(u64, u64)>,
    #[serde(default)]
    reported_task_versions: HashMap<i64, i64>,
    #[serde(default)]
    result_policy: workspace::ResultPolicy,
    waiting: bool,
    #[serde(default)]
    wait_cursor: Option<WaitCursor>,
    blocked: bool,
    repaired: bool,
    #[serde(default)]
    wait_repaired: bool,
}

fn record_command(worker: &mut Worker, item: &Value, started: bool, sequence: Option<u64>) {
    let Some(id) = item["id"].as_str() else {
        return;
    };
    if started {
        if let Some(sequence) = sequence {
            worker.command_sequences.insert(id.into(), sequence);
        }
        if let Some(sequence) = sequence
            && let Some((_, revision)) = worker
                .revision_fences
                .iter()
                .rev()
                .find(|(boundary, _)| sequence > *boundary)
        {
            worker
                .command_revisions
                .entry(id.into())
                .or_insert(*revision);
        }
    } else {
        let mut record = item.clone();
        record["_delm_revision"] = json!(worker.command_revisions.remove(id));
        record["_delm_started_sequence"] = json!(worker.command_sequences.remove(id));
        record["_delm_order"] = json!(worker.checks.len());
        worker.checks.insert(id.into(), record);
    }
}

fn recent_commands(worker: &Worker, revision: u64) -> Value {
    let mut commands = worker
        .checks
        .iter()
        .filter(|(_, record)| record["_delm_revision"].as_u64() == Some(revision))
        .collect::<Vec<_>>();
    commands.sort_by_key(|(id, record)| {
        (
            std::cmp::Reverse(record["_delm_order"].as_u64().unwrap_or(0)),
            id.as_str(),
        )
    });
    json!(commands.into_iter().take(12).map(|(id, record)| json!({
        "id": id, "command": record["command"], "status": record["status"], "exitCode": record["exitCode"]
    })).collect::<Vec<_>>())
}

fn task_event(worker: &mut Worker, index: usize, tool: &str, body: &Value) -> Option<Event> {
    let verb = match tool {
        "delm_task_claim" => "claimed",
        "delm_task_finish" => "finished",
        "delm_task_release" => "released",
        "delm_task_split" => "split",
        _ => return None,
    };
    let result = &body["result"];
    let id = result["task_id"].as_i64()?;
    let version = result["version"].as_i64()?;
    if worker
        .reported_task_versions
        .get(&id)
        .is_some_and(|previous| *previous >= version)
    {
        return None;
    }
    worker.reported_task_versions.insert(id, version);
    let title = body["board"]["tasks"]
        .as_array()
        .and_then(|tasks| tasks.iter().find(|task| task["task_id"] == id))
        .and_then(|task| task["title"].as_str());
    let label = title.map_or_else(|| format!("task #{id}"), |title| format!("#{id}: {title}"));
    let message = if tool == "delm_task_split" {
        let count = result["created"].as_array().map_or(0, Vec::len);
        format!(
            "Worker {} exposed {count} available tasks and is continuing {label}",
            index + 1
        )
    } else {
        format!("Worker {} {verb} {label}", index + 1)
    };
    let mut event = Event::new("status", message);
    event.id = Some(format!("task:{id}:{version}"));
    event.details = Some(
        json!({"worker":index+1,"action":tool.trim_start_matches("delm_"),
        "task_id":id,"version":version,"state":result["state"],"title":title,"created":result["created"]}),
    );
    Some(event)
}

#[derive(Serialize, Deserialize)]
struct Saved {
    request: StartRequest,
    workspace: PreparedWorkspace,
    workers: [Worker; 2],
    revision: u64,
    expires: u64,
    status: String,
}

pub async fn serve(
    input: mpsc::Receiver<HostCommand>,
    output: mpsc::UnboundedSender<Event>,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    serve_inner(input, output, cancel, None).await
}

/// Bound native invocations admit work as soon as authenticated control is
/// registered. Standalone CLI users establish their monitoring lease explicitly.
pub(crate) async fn serve_controlled(
    input: mpsc::Receiver<HostCommand>,
    output: mpsc::UnboundedSender<Event>,
    cancel: tokio::sync::watch::Receiver<bool>,
    admission: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    serve_inner(input, output, cancel, Some(admission)).await
}

async fn serve_inner(
    mut input: mpsc::Receiver<HostCommand>,
    output: mpsc::UnboundedSender<Event>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    admission: Option<tokio::sync::oneshot::Receiver<()>>,
) -> Result<()> {
    let first = input.recv().await.context("No start request received")?;
    let (mut saved, _lock) = match first {
        HostCommand::Start(request) => prepare(request, &mut input, &output).await?,
        HostCommand::Resume {
            run_id,
            authorization,
        } => {
            let dir = state::run_path(&run_id)?;
            let state: Saved = serde_json::from_slice(&fs::read(dir.join("run.json"))?)?;
            ensure!(
                state.status == "paused",
                "This run is not available to resume"
            );
            ensure!(
                state
                    .request
                    .auth_settings
                    .pointer("/account_identity/resume_supported")
                    .and_then(Value::as_bool)
                    != Some(false),
                "This account mode cannot be identified safely for resuming. Open the partial project and start a new task there."
            );
            ensure!(
                state.expires == 0 || state.expires > now(),
                "This run's execution allowance has expired. Its work remains available."
            );
            ensure!(
                authorization.project.canonicalize()? == state.workspace.original
                    && authorization.auth_home.canonicalize()? == state.request.auth_home
                    && authorization.model == state.request.model
                    && authorization.model_provider == state.request.model_provider
                    && authorization.reasoning_effort == state.request.reasoning_effort
                    && authorization.service_tier == state.request.service_tier
                    && authorization.policy == state.request.policy
                    && authorization.auth_settings == state.request.auth_settings,
                "The current project, account settings, model, or permissions changed. Open the partial work instead of resuming with stale authorization."
            );
            let lock = RunLock::acquire(&state.workspace.original)?;
            (state, lock)
        }
        _ => bail!("The first command must start or explicitly resume a DeLM task"),
    };
    let run_dir = saved.workspace.run_dir.clone();
    if let Some(mut admission) = admission {
        let mut event = Event::new(
            "awaiting_control",
            "Private workspaces are ready. Waiting for the conversation to confirm control access before starting workers.",
        );
        event.run_id = run_dir
            .file_name()
            .map(|id| id.to_string_lossy().into_owned());
        event.request_revision = Some(saved.revision);
        let _ = output.send(event);
        let admitted = 'admission: loop {
            if *cancel.borrow() {
                break false;
            }
            tokio::select! { biased;
                _ = cancel.changed() => break false,
                ready = &mut admission => {
                    if ready.is_err() { break false; }
                    // Include updates accepted before control confirmation even
                    // when both channels become ready in the same scheduler turn.
                    while let Ok(command) = input.try_recv() {
                        match command {
                            HostCommand::Stop => break 'admission false,
                            HostCommand::Message { text, attachments } => {
                                saved.request.attachments.extend(attachments);
                                saved.revision += 1;
                                saved.request.task.push_str(&format!("\n\nUser update:\n{text}"));
                            }
                            _ => {}
                        }
                    }
                    break true;
                },
                command = input.recv() => match command {
                    Some(HostCommand::Stop) | None => break false,
                    Some(HostCommand::Message { text, attachments }) => {
                        saved.request.attachments.extend(attachments);
                                saved.revision += 1;
                        saved.request.task.push_str(&format!("\n\nUser update:\n{text}"));
                    }
                    _ => {}
                }
            }
        };
        if !admitted {
            let recovery = workspace::preserve_partial_and_cleanup(&saved.workspace)?;
            saved.status = "stopped".into();
            atomic_json(&run_dir.join("run.json"), &saved)?;
            let mut stopped = Event::new(
                "stopped",
                "DeLM stopped before worker startup. No task model turn was started.",
            );
            stopped.run_id = run_dir
                .file_name()
                .map(|id| id.to_string_lossy().into_owned());
            stopped.request_revision = Some(saved.revision);
            stopped.partial_paths = vec![recovery.recovery.clone()];
            stopped.details = Some(serde_json::to_value(recovery)?);
            let _ = output.send(stopped);
            return Ok(());
        }
        if let Err(error) = atomic_json(&run_dir.join("run.json"), &saved) {
            return Err(startup_failure(&mut saved, &output, error));
        }
    }
    let startup = (|| -> Result<_> {
        let mut journal = Journal::open(&run_dir)?;
        journal.record(
            "started",
            &json!({"deadline":saved.expires,"revision":saved.revision}),
        )?;
        let mut board = Board::open(
            &run_dir,
            &saved.workspace.baseline,
            saved.workspace.workers.clone(),
        )?;
        board.set_revision(saved.revision)?;
        Ok((journal, board))
    })();
    let (mut journal, mut board) = match startup {
        Ok(state) => state,
        Err(error) => {
            return Err(startup_failure(&mut saved, &output, error));
        }
    };
    let mut rpc = match RpcClient::spawn(&saved.request, &run_dir).await {
        Ok(rpc) => rpc,
        Err(error) => {
            return Err(startup_failure(&mut saved, &output, error));
        }
    };
    if saved.expires == 0 {
        saved.expires = now() + saved.request.seconds;
    }
    // Capture the absolute execution deadline before watchdog setup. Starting a
    // full-seconds sleep afterward would outlive this persisted wall deadline.
    let remaining = (UNIX_EPOCH + Duration::from_secs(saved.expires))
        .duration_since(SystemTime::now())
        .unwrap_or_default();
    let execution_deadline = tokio::time::Instant::now() + remaining;
    let guard = match crate::supervisor::Guard::start_with_paths(
        std::process::id(),
        rpc.pid,
        saved.expires * 1000 + WATCHDOG_HANDOFF_GRACE.as_millis() as u64,
        &rpc.launch_dir,
        std::slice::from_ref(&run_dir),
    ) {
        Ok(guard) => guard,
        Err(error) => {
            // No thread has been created; reap the metadata host before cleanup.
            if rpc.shutdown(&[]).await.is_ok() {
                cleanup_before_workers(&mut saved, &output)?;
            } else {
                saved.status = "recovery_required".into();
                atomic_json(&run_dir.join("run.json"), &saved)?;
            }
            return Err(error.context("Could not supervise workers; no task was started"));
        }
    };
    let result = if *cancel.borrow() {
        Ok(None)
    } else {
        tokio::select! { biased;
            _ = cancel.changed() => Ok(None),
            _ = tokio::time::sleep_until(execution_deadline) => Err(anyhow::anyhow!(TIME_LIMIT_REACHED)),
            result = async {rpc.initialize().await?; rpc.verify_account(&saved.request).await?; drive(&mut saved, &mut rpc, &mut board, &mut journal, &mut input, &output).await} => result,
        }
    };
    let threads = saved
        .workers
        .iter()
        .map(|w| (w.thread.clone(), w.turn.clone()))
        .collect::<Vec<_>>();
    journal.record("shutdown_started", &json!({"revision":saved.revision}))?;
    journal.observe("phase", &json!({"phase":"shutdown","boundary":"start"}))?;
    let shutdown_started = guard.begin_shutdown();
    let shutdown = rpc.shutdown(&threads).await.and(shutdown_started);
    let supervision = guard.finish();
    journal.record(
        "shutdown_finished",
        &json!({"native_acknowledged":shutdown.is_ok()}),
    )?;
    journal.observe(
        "phase",
        &json!({"phase":"shutdown","boundary":"end","success":shutdown.is_ok()}),
    )?;
    if shutdown.is_ok() && supervision.as_ref().is_ok_and(|report| report.clean()) {
        for (index, worker) in saved.workers.iter().enumerate() {
            if let Some(turn) = &worker.turn {
                journal.observe("worker_turn_finished", &json!({"worker":index+1,"turn_id":turn,"status":"stopped","revision":worker.revision}))?;
            }
        }
    }
    saved.workers.iter_mut().for_each(|w| w.turn = None);
    let mut result = result;
    if *cancel.borrow() {
        result = Ok(None);
    }
    while let Ok(command) = input.try_recv() {
        match command {
            HostCommand::Message { text, attachments } => {
                saved.request.attachments.extend(attachments);
                saved.revision += 1;
                saved
                    .request
                    .task
                    .push_str(&format!("\n\nUser update:\n{text}"));
                journal.record(
                    "late_user_update",
                    &json!({"revision":saved.revision,"text":text}),
                )?;
                result = Err(anyhow::anyhow!(
                    "Your update arrived while the workers were stopping. It is saved with the recovery bundle; the proposed result was not delivered"
                ));
            }
            HostCommand::Answer { id, answers } => {
                journal.record("unapplied_user_answer", &json!({"id":id,"answers":answers}))?;
                saved.request.context.push_str(&format!(
                    "\n\nAnswer received during shutdown and not applied: {}",
                    serde_json::to_string(&answers)?
                ));
                result = Err(anyhow::anyhow!(
                    "Your answer arrived during shutdown. It is preserved with the partial work but was not applied"
                ));
            }
            HostCommand::Stop => result = Ok(None),
            _ => {}
        }
    }
    let writers_stopped =
        shutdown.is_ok() && supervision.as_ref().is_ok_and(|report| report.clean());
    let mut delivery_failed = false;
    let result = match result {
        Ok(Some((winner, candidate))) => {
            journal.observe(
                "phase",
                &json!({"phase":"delivery_and_cleanup","boundary":"start"}),
            )?;
            let delivery = (|| -> Result<workspace::DeliveryReport> {
                ensure!(
                    writers_stopped,
                    "Worker processes did not stop cleanly; both projects are preserved"
                );
                candidate.verify(&saved.workspace.workers[winner])?;
                atomic_json(&run_dir.join("completion.json"), &candidate)?;
                // No owned writer remains before delivery and workspace cleanup.
                workspace::deliver_result(
                    &saved.workspace,
                    winner,
                    &saved.workers[winner].result_policy,
                )
            })();
            journal.observe("phase", &json!({"phase":"delivery_and_cleanup","boundary":"end","success":delivery.is_ok()}))?;
            match delivery {
                Ok(delivery) => Ok(Some((winner, delivery))),
                Err(error) => {
                    delivery_failed = true;
                    Err(error.context("Result delivery requires recovery"))
                }
            }
        }
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    };
    match result {
        Ok(Some((winner, delivery))) => {
            saved.status = if !delivery.delivered {
                "delivery_conflict"
            } else if delivery.verification_required {
                "delivered"
            } else {
                "complete"
            }
            .into();
            atomic_json(&run_dir.join("run.json"), &saved)?;
            let mut event = Event::new(
                if !delivery.delivered {
                    "stopped"
                } else if delivery.verification_required {
                    "delivered"
                } else {
                    "result"
                },
                saved.workers[winner]
                    .outcome
                    .as_ref()
                    .and_then(|v| v.get("summary"))
                    .and_then(Value::as_str)
                    .unwrap_or("Changes have been delivered to your project."),
            );
            if !delivery.delivered {
                event.message = format!(
                    "Delivery needs conflict resolution for: {}. Your edits and the proposed changes are preserved.",
                    delivery.conflicts.join(", ")
                );
            } else if delivery.verification_required {
                event.message.push_str(" Changes are in your project. Run the necessary local setup or focused check for the merged/relocated result before reporting it ready.");
            }
            event.path = Some(delivery.project.clone());
            if let Some(recovery) = &delivery.recovery {
                event.partial_paths.push(recovery.clone());
            }
            let completion: Value =
                serde_json::from_slice(&fs::read(run_dir.join("completion.json"))?)?;
            event.request_revision = Some(saved.revision);
            event.details = Some(json!({"worker":winner+1,"delivery":delivery,
                "completion":run_dir.join("completion.json"),"checks":completion["checks"],
                "shared_checks":completion["shared_checks"],"previews_stopped":true}));
            event.run_id = run_dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned());
            let _ = output.send(event);
            journal.record(&saved.status, &json!({"worker":winner+1}))?;
        }
        other => {
            journal.observe(
                "phase",
                &json!({"phase":"recovery_and_cleanup","boundary":"start"}),
            )?;
            let recovery = if writers_stopped {
                workspace::preserve_partial_and_cleanup(&saved.workspace)
            } else {
                Err(anyhow::anyhow!(
                    "Worker shutdown or result delivery needs recovery"
                ))
            };
            journal.observe("phase", &json!({"phase":"recovery_and_cleanup","boundary":"end","success":recovery.is_ok()}))?;
            saved.status = if recovery.is_ok() {
                if delivery_failed {
                    "delivery_conflict"
                } else {
                    "stopped"
                }
            } else {
                "recovery_required"
            }
            .into();
            atomic_json(&run_dir.join("run.json"), &saved)?;
            let message = match other {
                Err(error) => format!("DeLM stopped: {error:#}. Partial work is preserved."),
                _ => "DeLM stopped. Partial work is preserved.".into(),
            };
            let mut event = Event::new(
                "stopped",
                if writers_stopped {
                    message
                } else {
                    format!(
                        "{message} Process cleanup is unresolved, so resuming and cleanup are disabled. Run records: {}",
                        run_dir.display()
                    )
                },
            );
            match recovery {
                Ok(report) => {
                    event.partial_paths = vec![report.recovery.clone()];
                    event.details = Some(serde_json::to_value(report)?);
                }
                Err(error) => {
                    event
                        .message
                        .push_str(&format!(" Cleanup requires recovery: {error:#}."));
                }
            }
            event.run_id = run_dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned());
            let _ = output.send(event);
            journal.record(&saved.status, &json!({"shutdown_ok":shutdown.is_ok()}))?;
        }
    }
    Ok(())
}

fn startup_failure(
    saved: &mut Saved,
    output: &mpsc::UnboundedSender<Event>,
    error: anyhow::Error,
) -> anyhow::Error {
    match cleanup_before_workers(saved, output) {
        Ok(()) => error,
        Err(cleanup) => error.context(format!(
            "Startup failed and cleanup needs recovery at {}: {cleanup:#}",
            saved.workspace.run_dir.display()
        )),
    }
}

fn cleanup_before_workers(saved: &mut Saved, output: &mpsc::UnboundedSender<Event>) -> Result<()> {
    let recovery = workspace::preserve_partial_and_cleanup(&saved.workspace)?;
    saved.status = "stopped".into();
    atomic_json(&saved.workspace.run_dir.join("run.json"), saved)?;
    let mut event = Event::new(
        "stopped",
        "Startup stopped before any task turn. Temporary workspaces were removed.",
    );
    event.run_id = saved
        .workspace
        .run_dir
        .file_name()
        .map(|id| id.to_string_lossy().into_owned());
    event.partial_paths = vec![recovery.recovery.clone()];
    event.details = Some(serde_json::to_value(recovery)?);
    let _ = output.send(event);
    Ok(())
}

fn announce_ready(
    candidate: Option<&(usize, completion::Completion)>,
    revision: u64,
    questions: &questions::Questions,
    approvals: &approvals::Approvals,
    output: &mpsc::UnboundedSender<Event>,
) {
    if candidate.is_some() && questions.is_empty() && approvals.is_empty() {
        let mut ready = Event::new(
            "ready",
            "The shared result is ready. Stopping workers before delivery.",
        );
        ready.request_revision = Some(revision);
        let _ = output.send(ready);
    }
}

async fn prepare(
    mut request: StartRequest,
    input: &mut mpsc::Receiver<HostCommand>,
    output: &mpsc::UnboundedSender<Event>,
) -> Result<(Saved, RunLock)> {
    ensure!(
        !request.task.trim().is_empty(),
        "Enter the task after $delm:run"
    );
    ensure!(
        request.seconds > 0 && request.seconds <= 24 * 60 * 60,
        "Execution allowance must be between one second and 24 hours"
    );
    request.project = request
        .project
        .canonicalize()
        .context("The selected project is unavailable")?;
    request.auth_home = request
        .auth_home
        .canonicalize()
        .context("Native Codex account directory is unavailable")?;
    ensure!(
        !request.auth_home.starts_with(&request.project)
            && !request.project.starts_with(&request.auth_home),
        "Native Codex account storage must be separate from the original project. Choose a repository outside that directory."
    );
    request.host_executable = request.host_executable.canonicalize()?;
    state::check_storage_boundary(&request.project)?;
    let lock = RunLock::acquire(&request.project)?;
    let run_dir = state::create_run()?;
    let selected_project = request.project.clone();
    atomic_json(
        &run_dir.join("run.json"),
        &json!({
            "host":"codex", "status":"preparing", "project":selected_project,
            "native_started":false, "finished":false, "workspace_cleanup_complete":false
        }),
    )?;
    let prepared = prepare_capture(request, input, output, &run_dir).await;
    match prepared {
        Ok(saved) => Ok((saved, lock)),
        Err(error) => {
            if let Err(record_error) =
                record_preparation_failure(&run_dir, &selected_project, &error)
            {
                return Err(error.context(format!(
                    "Preparation state needs inspection at {}: {record_error:#}",
                    run_dir.display()
                )));
            }
            Err(error)
        }
    }
}

async fn prepare_capture(
    mut request: StartRequest,
    input: &mut mpsc::Receiver<HostCommand>,
    output: &mpsc::UnboundedSender<Event>,
    run_dir: &std::path::Path,
) -> Result<Saved> {
    ensure!(
        !run_dir.starts_with(&request.project),
        "Choose a repository, not the directory containing DeLM's private storage"
    );
    let mut preparing = Event::new(
        "status",
        "Preparing two working copies. The completed changes will be applied to your project.",
    );
    preparing.run_id = run_dir
        .file_name()
        .map(|id| id.to_string_lossy().into_owned());
    preparing.request_revision = Some(1);
    let _ = output.send(preparing);
    fs::create_dir_all(run_dir.join("attachments"))?;
    request.attachments = crate::inputs::capture(&request.attachments, run_dir)?;
    let project = request.project.clone();
    let directory = run_dir.to_path_buf();
    request.auth_settings["invocation_task"] = json!(request.task);
    let mut preparation_journal = Journal::open(run_dir)?;
    preparation_journal.record(
        "preparation_started",
        &json!({"invocation_received_at_ms":request.auth_settings["invocation_received_at_ms"]}),
    )?;
    preparation_journal.observe("phase", &json!({"phase":"preparation","boundary":"start"}))?;
    let mut revision = 1;
    let mut prepare = tokio::task::spawn_blocking(move || {
        workspace::prepare(&project, &directory, crate::config::MAX_REPO_SIZE_BYTES)
    });
    let workspace = loop {
        tokio::select! {
            result = &mut prepare => {
                preparation_journal.observe("phase", &json!({"phase":"preparation","boundary":"end","success":result.as_ref().is_ok_and(|result| result.is_ok())}))?;
                break result??;
            },
            command = input.recv() => match command {
                Some(HostCommand::Stop) | None => {
                    // Capture is bounded and has no model work or source writes.
                    // Await it before returning so preparation cannot become an orphan.
                    if let Ok(Ok(workspace)) = prepare.await {
                        let recovery=workspace::preserve_partial_and_cleanup(&workspace)?;
                        record_preparation_cancelled(&workspace, &recovery)?;
                        let mut event=Event::new("stopped","Preparation cancelled; temporary workspaces removed.");
                        event.partial_paths=vec![recovery.recovery];
                        let _=output.send(event);
                    }
                    bail!("Preparation cancelled; no worker was started");
                }
                Some(HostCommand::Message{text, attachments}) => {
                    request.attachments.extend(attachments);
                    revision += 1;
                    request.task.push_str(&format!("\n\nUser update:\n{text}"));
                }
                _ => {}
            }
        }
    };
    let mut saved = Saved {
        request,
        workspace,
        workers: Default::default(),
        revision,
        expires: 0,
        status: "prepared".into(),
    };
    if let Err(error) = preparation_journal
        .record("workspaces_prepared", &json!({"revision":revision}))
        .and_then(|()| atomic_json(&run_dir.join("run.json"), &saved))
    {
        return Err(startup_failure(&mut saved, output, error));
    }
    Ok(saved)
}

fn record_preparation_failure(
    run_dir: &std::path::Path,
    project: &std::path::Path,
    error: &anyhow::Error,
) -> Result<()> {
    let record: Value = serde_json::from_slice(&fs::read(run_dir.join("run.json"))?)?;
    // Once a complete Saved record exists, startup_failure owns its recovery
    // state. Never replace that evidence with a pre-admission conclusion.
    if record.get("workspace").is_some()
        || (record["status"] == "stopped"
            && record["native_started"] == false
            && record["finished"] == true
            && record["workspace_cleanup_complete"] == true)
    {
        return Ok(());
    }
    ensure!(
        record["native_started"] == false && record["status"] == "preparing",
        "Preparation record changed"
    );
    let captures = run_dir.join("workspace");
    let clean = match fs::symlink_metadata(&captures) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::read_dir(&captures)?.all(|entry| {
                entry.is_ok_and(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            })
        }
        _ => false,
    };
    atomic_json(
        &run_dir.join("run.json"),
        &json!({
            "host":"codex", "status":"preparation_failed", "project":project,
            "native_started":false, "finished":clean, "workspace_cleanup_complete":clean,
            "reason":error.to_string()
        }),
    )
}

fn record_preparation_cancelled(
    workspace: &PreparedWorkspace,
    recovery: &workspace::RecoveryReport,
) -> Result<()> {
    ensure!(
        recovery.cleanup_complete,
        "Preparation cleanup is not complete"
    );
    atomic_json(
        &workspace.run_dir.join("run.json"),
        &json!({
            "host":"codex", "status":"stopped", "project":workspace.original,
            "native_started":false, "finished":true, "workspace_cleanup_complete":true,
            "recovery":recovery
        }),
    )
}

async fn start_turn(
    rpc: &RpcClient,
    worker: &mut Worker,
    revision: u64,
    text: &str,
    attachments: &[Value],
    journal: &mut Journal,
    index: usize,
) -> Result<()> {
    let mut input = vec![text_input(text)];
    input.extend_from_slice(attachments);
    let result = rpc
        .request(
            "turn/start",
            json!({"threadId":worker.thread,"input":input}),
        )
        .await?;
    worker.turn = Some(
        result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .context("Codex omitted turn identity")?
            .into(),
    );
    worker.revision = revision;
    worker.command_revisions.clear();
    worker.command_sequences.clear();
    worker.revision_fences = vec![(0, revision)];
    worker.outcome = None;
    worker.waiting = false;
    worker.wait_cursor = None;
    worker.blocked = false;
    journal.observe(
        "worker_turn_started",
        &json!({"worker":index+1,"turn_id":worker.turn,"revision":worker.revision}),
    )?;
    Ok(())
}

async fn update_worker(
    rpc: &RpcClient,
    worker: &mut Worker,
    revision: u64,
    text: &str,
    attachments: &[Value],
    journal: &mut Journal,
    index: usize,
) -> Result<()> {
    let message = format!("User update, revision {revision}: {text}");
    let mut input = vec![text_input(&message)];
    input.extend_from_slice(attachments);
    let Some(turn) = worker.turn.clone() else {
        return start_turn(rpc, worker, revision, &message, attachments, journal, index).await;
    };
    if rpc
        .request(
            "turn/steer",
            json!({"threadId":worker.thread,"expectedTurnId":turn,"input":input}),
        )
        .await
        .is_ok()
    {
        worker.revision = revision;
        worker
            .revision_fences
            .push((rpc.received_sequence(), revision));
        return Ok(());
    }
    let current = rpc
        .request(
            "thread/read",
            json!({"threadId":worker.thread,"includeTurns":true}),
        )
        .await?;
    let latest = current
        .pointer("/thread/turns")
        .and_then(Value::as_array)
        .and_then(|turns| turns.last())
        .context("Cannot reconcile the native turn after the update")?;
    ensure!(
        latest["id"].as_str() == Some(turn.as_str())
            && matches!(
                latest["status"].as_str(),
                Some("completed" | "interrupted" | "failed")
            ),
        "The update could not be acknowledged safely. No duplicate turn was started."
    );
    journal.observe(
        "worker_turn_finished",
        &json!({
            "worker":index+1, "turn_id":turn, "revision":worker.revision,
            "status":latest["status"], "waiting":false, "source":"native_state_reconciliation"
        }),
    )?;
    worker.turn = None;
    start_turn(rpc, worker, revision, &message, attachments, journal, index).await
}

fn initial_turn_input(request: &StartRequest, revision: u64) -> (String, Vec<Value>) {
    let mut task = format!(
        "User request (revision {}):\n{}\n\nRelevant context supplied by the parent conversation:\n{}\n\nYour project is the private working directory. Start useful work now.",
        revision, request.task, request.context
    );
    let mut initial_inputs = request.attachments.clone();
    if let Some(native) = request.auth_settings["invocation_inputs"].as_array() {
        task = format!(
            "Execute the original user input below (request revision {}). DeLM is already running; do not invoke DeLM again. Use your inherited conversation context and ordinary Codex capabilities. Relevant additional context: {}",
            revision, request.context
        );
        if let Some(original) = request.auth_settings["invocation_task"].as_str()
            && let Some(updates) = request.task.strip_prefix(original)
            && !updates.is_empty()
        {
            task.push_str(updates);
        }
        // Preserve native text elements and attachment offsets byte-for-byte.
        // The wrapper consumes the DeLM invocation without rewriting user input.
        let delm_skill = request.auth_settings["delm_skill_path"]
            .as_str()
            .and_then(|path| PathBuf::from(path).canonicalize().ok());
        initial_inputs.extend(
            native
                .iter()
                .filter(|item| {
                    !(item["type"] == "skill"
                        && delm_skill.as_ref().is_some_and(|own| {
                            item["path"]
                                .as_str()
                                .and_then(|path| PathBuf::from(path).canonicalize().ok())
                                .as_ref()
                                == Some(own)
                        }))
                })
                .cloned(),
        );
    }
    (task, initial_inputs)
}

async fn drive(
    saved: &mut Saved,
    rpc: &mut RpcClient,
    board: &mut Board,
    journal: &mut Journal,
    input: &mut mpsc::Receiver<HostCommand>,
    output: &mpsc::UnboundedSender<Event>,
) -> Result<Option<(usize, completion::Completion)>> {
    journal.observe(
        "phase",
        &json!({"phase":"worker_admission","boundary":"start"}),
    )?;
    let run_id = saved
        .workspace
        .run_dir
        .file_name()
        .context("Run identity missing")?
        .to_string_lossy();
    let mut gateway = crate::worker_tools::Gateway::start(&run_id)?;
    let mut services = crate::services::Services::default();
    crate::workers::prepare_worker_capabilities(rpc, &saved.request).await?;
    for index in 0..2 {
        let mut config = worker_config(
            &saved.request,
            &saved.workspace.run_dir,
            &saved.workspace.workers[index],
            index + 1,
        )?;
        board.set_worker_policy(index + 1, &config)?;
        let scopes = config["permissions"][format!("delm_worker_{}", index + 1)]["filesystem"]
            .as_object()
            .context("Worker filesystem policy missing")?;
        saved.workers[index].result_policy = workspace::ResultPolicy {
            native_python_runtime: false,
            readonly_runtime_roots: scopes
                .iter()
                .filter(|(path, access)| path.starts_with('/') && access.as_str() == Some("read"))
                .map(|(path, _)| PathBuf::from(path))
                .collect(),
            denied_roots: scopes
                .iter()
                .filter(|(path, access)| path.starts_with('/') && access.as_str() == Some("deny"))
                .map(|(path, _)| PathBuf::from(path))
                .collect(),
        };
        let instructions = format!(
            "{}\n\nYou are worker {}.\n\nThe original project path is {}. Interpret references beneath that path as the corresponding relative paths in your private working directory; never open the original path.",
            include_str!("../../plugin/worker.md"),
            index + 1,
            serde_json::to_string(&saved.workspace.original)?
        );
        config["mcp_servers"][format!("delm_coordination_{}", index + 1)] =
            gateway.config(index)?;
        let result = if saved.workers[index].thread.is_empty() {
            let (method, params) = crate::workers::worker_thread_request(
                &saved.request,
                &saved.workspace.workers[index],
                index + 1,
                config,
                &instructions,
            )?;
            rpc.request(method, params).await?
        } else {
            config
                .as_object_mut()
                .context("Worker configuration missing")?
                .remove("permissions");
            rpc.request("thread/resume", json!({"threadId":saved.workers[index].thread,"cwd":saved.workspace.workers[index],"model":saved.request.model,"modelProvider":saved.request.model_provider,"serviceTier":saved.request.service_tier,"config":config})).await?
        };
        verify_thread_response(
            &saved.request,
            &saved.workspace.run_dir,
            &saved.workspace.workers[index],
            index + 1,
            &result,
        )?;
        saved.workers[index].thread = result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .context("Codex omitted thread identity")?
            .into();
        let capabilities = crate::workers::verify_worker_capabilities(
            rpc,
            &saved.request,
            &saved.workspace.workers[index],
            &saved.workers[index].thread,
        )
        .await?;
        atomic_json(
            &saved
                .workspace
                .run_dir
                .join(format!("worker-{}-capabilities.json", index + 1)),
            &capabilities,
        )?;
        atomic_json(&saved.workspace.run_dir.join("run.json"), saved)?;
    }
    if saved.expires == 0 {
        saved.expires = now() + saved.request.seconds;
    }
    ensure!(saved.expires > now(), "The execution allowance has expired");
    saved.status = "running".into();
    let (task, initial_inputs) = initial_turn_input(&saved.request, saved.revision);
    for (index, worker) in saved.workers.iter_mut().enumerate() {
        journal.record(
            "worker_turn_requested",
            &json!({"worker":index+1,"revision":saved.revision}),
        )?;
        start_turn(
            rpc,
            worker,
            saved.revision,
            &task,
            &initial_inputs,
            journal,
            index,
        )
        .await?;
    }
    journal.observe(
        "phase",
        &json!({"phase":"worker_admission","boundary":"end","success":true}),
    )?;
    atomic_json(&saved.workspace.run_dir.join("run.json"), saved)?;
    let mut started = Event::new(
        "started",
        "Two DeLM workers are running and sharing progress.",
    );
    started.request_revision = Some(saved.revision);
    let _ = output.send(started);
    let mut first_actions = [false; 2];
    let mut questions = questions::Questions::default();
    let mut approvals = approvals::Approvals::default();
    let mut pending_candidate: Option<(usize, completion::Completion)> = None;
    let mut pending_accept_revision = None;
    let mut tool_calls = tool_calls::Calls::default();
    loop {
        if input.is_empty() && rpc.events.is_empty() {
            for call in tool_calls.take_ready(&saved.workers) {
                let (success, body) = dispatch_tool(
                    saved,
                    rpc,
                    board,
                    &mut services,
                    call.worker,
                    &call.tool,
                    call.arguments,
                    output,
                    pending_candidate.is_none(),
                    journal,
                )
                .await?;
                let text = serde_json::to_string(&body)?;
                journal.observe(
                    "coordination_response",
                    &json!({"worker":call.worker+1,"bytes":text.len()}),
                )?;
                let _ = call
                    .reply
                    .send(json!({"isError":!success,"content":[{"type":"text","text":text}]}));
                atomic_json(&saved.workspace.run_dir.join("run.json"), saved)?;
            }
        }
        // Drain already-received native requests before accepting. An approval
        // can arrive after `ready` while the host acknowledgment is queued.
        if pending_accept_revision == Some(saved.revision)
            && pending_candidate
                .as_ref()
                .is_some_and(|(_, candidate)| candidate.revision == saved.revision)
            && questions.is_empty()
            && approvals.is_empty()
            && input.is_empty()
            && rpc.events.is_empty()
        {
            return Ok(pending_candidate.take());
        }
        let remaining = saved.expires.saturating_sub(now());
        if remaining == 0 {
            bail!(TIME_LIMIT_REACHED);
        }
        let mut persist = true;
        tokio::select! { biased;
            command = input.recv() => match command {
                None | Some(HostCommand::Stop) => return Ok(None),
                Some(HostCommand::Message{text, attachments}) => {
                    pending_candidate = None;
                    pending_accept_revision = None;
                    saved.revision += 1; board.set_revision(saved.revision)?;
                    let mut accepted = Event::new("status", "Applying your update to both workers.");
                    accepted.request_revision = Some(saved.revision);
                    let _ = output.send(accepted);
                    saved.request.task.push_str(&format!("\n\nUser update:\n{text}"));
                    journal.record("user_update", &json!({"revision":saved.revision,"text":text}))?;
                    saved.request.attachments.extend(attachments.clone());
                    for (index, worker) in saved.workers.iter_mut().enumerate() {
                        worker.outcome = None;
                        update_worker(rpc,worker,saved.revision,&text,&attachments,journal,index).await?;
                    }
                }
                Some(HostCommand::Answer { id, answers }) => {
                    let pending = match questions.take(&id, &answers) {
                        Ok(pending) => pending,
                        Err(error) => {
                            let mut event = Event::new("answer_rejected", error.to_string()); event.id = Some(id);
                            let _ = output.send(event);
                            announce_ready(pending_candidate.as_ref(), saved.revision, &questions, &approvals, output);
                            continue;
                        }
                    };
                    if saved.workers[pending.worker].turn.as_deref() != Some(&pending.turn) {
                        let mut event = Event::new("answer_rejected", "This question belonged to a completed turn; your answer was not applied.");
                        event.id = Some(id); let _ = output.send(event);
                        announce_ready(pending_candidate.as_ref(), saved.revision, &questions, &approvals, output);
                        continue;
                    }
                    let response = answers.iter().map(|(key, values)| (key.clone(), json!({"answers":values}))).collect::<serde_json::Map<_,_>>();
                    rpc.respond(pending.native_id, json!({"answers":response})).await?;
                    let _ = output.send(questions::resolved(&id));
                    pending_candidate = None;
                    pending_accept_revision = None;
                    saved.revision += 1;
                    board.set_revision(saved.revision)?;
                    let text = format!("User answered these questions: {}\nAnswers: {}", pending.items, serde_json::to_string(&answers)?);
                    saved.request.task.push_str(&format!("\n\n{text}"));
                    journal.record("user_answer", &json!({"id":id,"revision":saved.revision,"questions":pending.items,"answers":answers}))?;
                    let mut event = Event::new("answer_applied", "Applying your answer to both workers.");
                    event.id = Some(id);
                    event.request_revision = Some(saved.revision);
                    let _ = output.send(event);
                    for (index, worker) in saved.workers.iter_mut().enumerate() {
                        worker.outcome = None;
                        update_worker(rpc,worker,saved.revision,&text,&[],journal,index).await?;
                    }
                }
                Some(HostCommand::Respond{id,response}) => {
                    match approvals.take(&id,&response) {
                        Ok(pending) => {
                            if pending.turn.as_deref().is_none_or(|turn|saved.workers[pending.worker].turn.as_deref()==Some(turn)) {
                                rpc.respond(pending.native_id,response).await?;
                                let mut event=Event::new("approval_resolved","Your response was sent to Codex."); event.id=Some(id); let _=output.send(event);
                            } else {
                                let mut event=Event::new("approval_resolved","The native request ended before this response; it was not applied."); event.id=Some(id); let _=output.send(event);
                            }
                        },
                        Err(error) => {let _=output.send(Event::new("notice",format!("Native response was not applied: {error}")));}
                    }
                    announce_ready(pending_candidate.as_ref(), saved.revision, &questions, &approvals, output);
                },
                Some(HostCommand::Approval{..}) => {},
                Some(HostCommand::AcceptResult{request_revision})
                    if request_revision == saved.revision
                        && pending_candidate.as_ref().is_some_and(|(_,candidate)|candidate.revision == request_revision) => {
                    pending_accept_revision = Some(request_revision);
                }
                _ => {}
            },
            message = rpc.events.recv() => {
                let message = message.context("Worker event stream closed")?;
                journal.record("native", &message)?;
                let method = message.get("method").and_then(Value::as_str).unwrap_or("");
                persist = matches!(method, "item/tool/call" | "item/started" | "item/completed" | "turn/completed");
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                if method.starts_with("delm/transport") { bail!("The native worker connection closed"); }
                if method == "serverRequest/resolved" {
                    if let Some(event) = questions.resolve(&params["requestId"]) { let _ = output.send(event); }
                    if let Some(event) = approvals.resolve(&params["requestId"]) { let _ = output.send(event); }
                    announce_ready(pending_candidate.as_ref(), saved.revision, &questions, &approvals, output);
                    continue;
                }
                let identity = params.get("threadId").and_then(Value::as_str).and_then(|id|saved.workers.iter().position(|w|w.thread == id));
                let Some(index) = identity else { if let Some(id) = message.get("id") { rpc.reject(id.clone(),"Unbound worker thread").await?; } continue; };
                if !first_actions[index] && matches!(method, "item/started" | "item/tool/call") {
                    first_actions[index]=true;
                    journal.record("worker_first_action", &json!({"worker":index+1,"method":method}))?;
                }
                match method {
                    "item/tool/call" => {
                        let id = message.get("id").context("Tool request identity missing")?.clone();
                        if params.get("turnId").and_then(Value::as_str) != saved.workers[index].turn.as_deref() {
                            rpc.reject(id,"This tool call belongs to a retired turn").await?; continue;
                        }
                        let tool = params.get("tool").and_then(Value::as_str).unwrap_or("");
                        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                        let (success,body) = dispatch_tool(saved,rpc,board,&mut services,index,tool,args,output,pending_candidate.is_none(),journal).await?;
                        let text=serde_json::to_string(&body)?;
                        journal.observe("coordination_response", &json!({"worker":index+1,"bytes":text.len()}))?;
                        rpc.respond(id,json!({"contentItems":[{"type":"inputText","text":text}],"success":success})).await?;
                    }
                    "item/started" => {
                        tool_calls.observe(index, &saved.workers[index], &params, message["_delm_received_sequence"].as_u64(), true)?;
                        if let Some(item) = params.get("item")
                            && item["type"].as_str() == Some("commandExecution")
                            && params["turnId"].as_str() == saved.workers[index].turn.as_deref()
                            && let Some(id) = item["id"].as_str() {
                            let _ = id;
                            record_command(&mut saved.workers[index], item, true, message["_delm_received_sequence"].as_u64());
                        }
                    }
                    "item/completed" => {
                        tool_calls.observe(index, &saved.workers[index], &params, None, false)?;
                        if let Some(item) = params.get("item")
                            && item.get("type").and_then(Value::as_str) == Some("commandExecution")
                            && params.get("turnId").and_then(Value::as_str) == saved.workers[index].turn.as_deref()
                            && let Some(id) = item.get("id").and_then(Value::as_str) {
                            let _ = id;
                            record_command(&mut saved.workers[index], item, false, None);
                        }
                    }
                    "turn/completed" => {
                        let turn = params.get("turn").context("Turn result missing")?;
                        if saved.workers[index].turn.as_deref() != turn.get("id").and_then(Value::as_str) { continue; }
                        journal.observe("worker_turn_finished", &json!({"worker":index+1,"turn_id":turn["id"],"revision":saved.workers[index].revision,"status":turn["status"],"waiting":turn["status"]=="completed" && saved.workers[index].revision==saved.revision && saved.workers[index].outcome.as_ref().is_some_and(|value|value["outcome"]=="waiting")}))?;
                        tool_calls.retire(index);
                        for event in questions.retire_turn(index, turn["id"].as_str().unwrap_or("")) { let _ = output.send(event); }
                        for event in approvals.retire_turn(index, turn["id"].as_str().unwrap_or("")) { let _ = output.send(event); }
                        saved.workers[index].turn = None;
                        let status = turn.get("status").and_then(Value::as_str).unwrap_or("");
                        if status != "completed" {
                            saved.workers[index].blocked = true;
                            for event in approvals.retire_worker(index) { let _=output.send(event); }
                            let released=board.release_worker_claims(index+1,status)?;
                            let retired_services=services.retire_worker(index+1)?;
                            journal.record("worker_services_retired", &retired_services)?;
                            let _=output.send(Event::new("notice",format!("Worker {} stopped with status {status}. {} tasks are available for its peer.",index+1,released.len())));
                        }
                        else if saved.workers[index].revision == saved.revision {
                            let outcome = saved.workers[index].outcome.clone();
                            match outcome.as_ref().and_then(|o|o.get("outcome")).and_then(Value::as_str) {
                                Some("complete") => {
                                    // Keep the result even when unrelated host responses are queued.
                                    // A task update invalidates it before acceptance.
                                    if pending_candidate.is_none() {
                                        journal.record("candidate",&json!({"worker":index+1,"revision":saved.revision,"declaration":outcome,"commands":saved.workers[index].checks}))?;
                                        atomic_json(&saved.workspace.run_dir.join("run.json"),saved)?;
                                        let declaration = outcome.as_ref().context("Completion declaration missing")?;
                                        let shared = board.shared_checks(index+1,declaration,saved.revision)?;
                                        let candidate = completion::Completion::capture_with_shared(&saved.workspace.workers[index],declaration,&saved.workers[index].checks,saved.revision,&saved.workers[index].result_policy,shared)?;
                                        pending_candidate = Some((index,candidate));
                                    }
                                }
                                Some("partial") => {
                                    let summary = outcome.as_ref().and_then(|o|o.get("summary")).and_then(Value::as_str).unwrap_or("");
                                    start_turn(rpc,&mut saved.workers[index],saved.revision,&format!("Advance the shared result by addressing this unfinished requirement: {summary}. Read current ownership, take ready work, and reuse peer contributions. Integrate only if no peer owns assembly."),&[],journal,index).await?;
                                }
                                Some("waiting") => saved.workers[index].waiting = true,
                                Some("blocked") => {
                                    saved.workers[index].blocked = true;
                                    let reason=outcome.as_ref().and_then(|value|value.get("summary")).and_then(Value::as_str).unwrap_or("Your input is needed.");
                                    let _=output.send(Event::new("notice",format!("Worker {}: {reason}",index+1)));
                                }
                                _ if !saved.workers[index].repaired => {
                                    saved.workers[index].repaired = true;
                                    start_turn(rpc,&mut saved.workers[index],saved.revision,"State the whole-request outcome with delm_complete. If a concrete requested requirement remains, finish it. If the result is ready, declare complete with the checks already performed; do not start another improvement pass.",&[],journal,index).await?;
                                }
                                _ => saved.workers[index].blocked = true,
                            }
                        }
                        if pending_candidate.is_none() {
                            wake_waiting(saved, rpc, board, journal).await?;
                        }
                        announce_ready(pending_candidate.as_ref(), saved.revision, &questions, &approvals, output);
                        if pending_candidate.is_none() && saved.workers.iter().all(|w|w.turn.is_none()) {
                            let repair_wait = saved.workers.iter().any(|w|w.waiting && !w.wait_repaired);
                            if repair_wait {
                                for (index, worker) in saved.workers.iter_mut().enumerate() {
                                    if worker.waiting && !worker.wait_repaired {
                                        worker.wait_repaired = true;
                                        start_turn(rpc,worker,saved.revision,"Your named dependency has no active producer. Read the board and resolve the dependency through useful work you can do now. If it requires user input, declare blocked with the concrete reason. Do not repeat the same wait.",&[],journal,index).await?;
                                    }
                                }
                            } else {return Ok(None);}
                        }
                    }
                    "item/tool/requestUserInput" => {
                        let id = message.get("id").context("Question identity missing")?.clone();
                        let turn = params["turnId"].as_str().context("Question turn missing")?;
                        if saved.workers[index].turn.as_deref() != Some(turn) {
                            rpc.reject(id,"Question belongs to a retired turn").await?;
                        } else {
                            match questions.insert(id.clone(), index, turn.into(), params["questions"].clone()) {
                                Ok(event) => { let _ = output.send(event); },
                                Err(error) => rpc.reject(id, &error.to_string()).await?,
                            }
                        }
                    }
                    "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" | "item/permissions/requestApproval" | "mcpServer/elicitation/request" => {
                        let id = message.get("id").context("Approval identity missing")?.clone();
                        if params["turnId"].as_str().is_some_and(|turn| saved.workers[index].turn.as_deref() != Some(turn)) {
                            rpc.reject(id,"Approval belongs to a retired turn").await?;
                            continue;
                        }
                        match approvals.insert(id.clone(),index,params["turnId"].as_str().map(str::to_owned),method,params.clone()) {
                            Ok(event) => {let _=output.send(event);},
                            Err(error) => rpc.reject(id,&error.to_string()).await?,
                        }
                    }
                    _ => { if let Some(id) = message.get("id") { rpc.reject(id.clone(),"This native capability is unavailable during DeLM").await?; } }
                }
            },
            call = gateway.calls.recv() => {
                let call = call.context("Worker tool transport closed")?;
                tool_calls.enqueue(call);
                persist = false;
            },
            _ = tokio::time::sleep(Duration::from_millis(100)), if tool_calls.has_pending() => { persist = false; },
            _ = tokio::time::sleep(Duration::from_secs(remaining)) => bail!(TIME_LIMIT_REACHED),
        }
        if persist {
            atomic_json(&saved.workspace.run_dir.join("run.json"), saved)?;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_tool(
    saved: &mut Saved,
    rpc: &mut RpcClient,
    board: &mut Board,
    services: &mut crate::services::Services,
    index: usize,
    tool: &str,
    args: Value,
    output: &mpsc::UnboundedSender<Event>,
    allow_wakeup: bool,
    journal: &mut Journal,
) -> Result<(bool, Value)> {
    let checked = if index >= 2 || saved.workers[index].turn.is_none() {
        Err(anyhow::anyhow!("This worker has no active turn"))
    } else if tool == "delm_complete" {
        completion::validate_checks(&args, &saved.workers[index].checks, saved.revision).map(|_| ())
    } else {
        Ok(())
    };
    let result = checked.and_then(|()| match tool {
        "delm_check_begin" => {
            board.begin_check(index + 1, args.clone(), || rpc.received_sequence())
        }
        "delm_check_finish" => {
            board.finish_check(index + 1, args.clone(), &saved.workers[index].checks)
        }
        "delm_service" => services.call(index + 1, args.clone(), rpc.pid, board),
        _ => board.call(index + 1, tool, args.clone()),
    });
    let success = result.is_ok();
    let mut body = match result {
        Ok(value) => value,
        Err(error) => json!({"error":error.to_string(),"board":board.view()?}),
    };
    if index >= 2 {
        return Ok((false, body));
    }
    body["recent_commands"] = recent_commands(&saved.workers[index], saved.revision);
    if success && tool == "delm_complete" {
        if saved.workers[index].outcome.as_ref() != Some(&args) {
            saved.workers[index].wait_cursor = board.wait_cursor(&args)?;
        }
        saved.workers[index].outcome = Some(args.clone());
    }
    if success && let Some(event) = task_event(&mut saved.workers[index], index, tool, &body) {
        let _ = output.send(event);
    }
    if success
        && matches!(tool, "delm_publish" | "delm_status")
        && let Some(summary) = args["summary"].as_str()
    {
        let _ = output.send(Event::new(
            "status",
            format!(
                "Worker {}: {}",
                index + 1,
                summary.chars().take(240).collect::<String>()
            ),
        ));
    }
    if success
        && allow_wakeup
        && matches!(
            tool,
            "delm_publish"
                | "delm_task_finish"
                | "delm_task_release"
                | "delm_task_split"
                | "delm_task_create"
        )
    {
        wake_waiting(saved, rpc, board, journal).await?;
    }
    Ok((success, body))
}

async fn wake_waiting(
    saved: &mut Saved,
    rpc: &RpcClient,
    board: &mut Board,
    journal: &mut Journal,
) -> Result<()> {
    for (index, worker) in saved.workers.iter_mut().enumerate() {
        if worker.waiting
            && !worker.blocked
            && worker.turn.is_none()
            && worker.revision == saved.revision
            && let Some(cursor) = &worker.wait_cursor
            && board.wait_ready(cursor)?
        {
            start_turn(rpc,worker,saved.revision,"New shared work is available. Read the board and claim useful ready work or continue your now-unblocked dependency.",&[],journal,index).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    #[test]
    fn cancellation_after_preparation_keeps_proven_cleanup_and_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("source.txt"), "Keep this source").unwrap();
        let prepared = workspace::prepare(&project, &temp.path().join("run"), 1_000_000).unwrap();
        let recovery = workspace::preserve_partial_and_cleanup(&prepared).unwrap();
        assert!(recovery.recovery.is_dir());
        record_preparation_cancelled(&prepared, &recovery).unwrap();
        let before = fs::read(prepared.run_dir.join("run.json")).unwrap();
        // The outer preparation error handler must preserve the successful
        // cleanup proof even though its recovery directory intentionally remains.
        record_preparation_failure(
            &prepared.run_dir,
            &prepared.original,
            &anyhow::anyhow!("Preparation cancelled; no worker was started"),
        )
        .unwrap();
        assert_eq!(fs::read(prepared.run_dir.join("run.json")).unwrap(), before);
        let saved: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(saved["status"], "stopped");
        assert_eq!(saved["finished"], true);
        assert_eq!(saved["workspace_cleanup_complete"], true);
        assert_eq!(saved["recovery"]["recovery"], json!(recovery.recovery));
        assert!(prepared.workers.iter().all(|path| !path.exists()));
        assert_eq!(
            fs::read_to_string(project.join("source.txt")).unwrap(),
            "Keep this source"
        );
    }

    #[test]
    fn failed_preparation_is_terminal_only_when_capture_cleanup_is_proven() {
        for remnants in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let run = temp.path().join("run");
            fs::create_dir(&run).unwrap();
            atomic_json(
                &run.join("run.json"),
                &json!({"status":"preparing","native_started":false}),
            )
            .unwrap();
            if remnants {
                fs::create_dir_all(run.join("workspace/capture-2")).unwrap();
            }
            record_preparation_failure(&run, temp.path(), &anyhow::anyhow!("Unsupported project"))
                .unwrap();
            let saved: Value =
                serde_json::from_slice(&fs::read(run.join("run.json")).unwrap()).unwrap();
            assert_eq!(saved["status"], "preparation_failed");
            assert_eq!(saved["native_started"], false);
            assert_eq!(saved["finished"], !remnants);
            assert_eq!(saved["workspace_cleanup_complete"], !remnants);
            assert_eq!(run.join("workspace/capture-2").exists(), remnants);
        }
        let temp = tempfile::tempdir().unwrap();
        let existing = json!({"status":"recovery_required","workspace":{"run_dir":temp.path()}});
        atomic_json(&temp.path().join("run.json"), &existing).unwrap();
        record_preparation_failure(temp.path(), temp.path(), &anyhow::anyhow!("Cleanup failed"))
            .unwrap();
        let saved: Value =
            serde_json::from_slice(&fs::read(temp.path().join("run.json")).unwrap()).unwrap();
        assert_eq!(saved, existing);
    }

    #[test]
    fn native_input_offsets_attachments_and_early_updates_survive_launch() {
        let temp = tempfile::tempdir().unwrap();
        let own = temp.path().join("delm-SKILL.md");
        let other = temp.path().join("user-SKILL.md");
        fs::write(&own, "DeLM").unwrap();
        fs::write(&other, "User skill").unwrap();
        let native = json!([
            {"type":"text","text":"$delm:run Build from [Image #1]","text_elements":[{"byte_range":{"start":21,"end":31},"placeholder":"[Image #1]"}]},
            {"type":"image","url":"data:image/png;base64,aGVsbG8="},
            {"type":"skill","name":"run","path":own},
            {"type":"skill","name":"run","path":other}
        ]);
        let request: StartRequest = serde_json::from_value(json!({
            "project":temp.path(), "task":"Build from [Image #1]\n\nUser update:\nKeep keyboard support",
            "context":"Previous discussion", "model":"fixture", "auth_home":temp.path(), "host_executable":"/fixture",
            "policy":{}, "attachments":[{"type":"text","text":"An additional selected reference"}],
            "auth_settings":{"invocation_inputs":native,"invocation_task":"Build from [Image #1]","delm_skill_path":own}
        })).unwrap();
        let (task, inputs) = initial_turn_input(&request, 2);
        assert!(task.contains("Keep keyboard support"));
        assert!(task.contains("Previous discussion"));
        assert_eq!(inputs.len(), 4);
        assert_eq!(inputs[0], request.attachments[0]);
        assert_eq!(inputs[1], native[0]);
        assert_eq!(inputs[2], native[1]);
        assert_eq!(inputs[3], native[3]);
    }

    #[test]
    fn command_finishing_after_steering_does_not_verify_the_new_request() {
        let mut worker = Worker {
            revision: 1,
            revision_fences: vec![(0, 1)],
            ..Default::default()
        };
        let command = json!({"id":"check", "type":"commandExecution", "command":"node --test", "cwd":"/private/project", "status":"completed", "exitCode":0});
        record_command(&mut worker, &command, true, Some(1));
        worker.revision = 2;
        record_command(&mut worker, &command, false, None);
        assert_eq!(worker.checks["check"]["_delm_revision"], 1);
        assert!(
            completion::validate_checks(
                &json!({"expected_revision":2,"checks":["check"]}),
                &worker.checks,
                2
            )
            .is_err()
        );
        assert!(recent_commands(&worker, 2).as_array().unwrap().is_empty());
        record_command(
            &mut worker,
            &json!({"id":"unknown-start","type":"commandExecution"}),
            false,
            None,
        );
        assert!(worker.checks["unknown-start"]["_delm_revision"].is_null());
        // This event arrived before the steer ACK but was processed after it.
        worker.revision_fences.push((10, 2));
        let queued = json!({"id":"queued-start","type":"commandExecution"});
        record_command(&mut worker, &queued, true, Some(8));
        record_command(&mut worker, &queued, false, None);
        assert_eq!(worker.checks["queued-start"]["_delm_revision"], 1);
        let fresh = json!({"id":"fresh-start","type":"commandExecution"});
        record_command(&mut worker, &fresh, true, Some(11));
        record_command(&mut worker, &fresh, false, None);
        assert_eq!(worker.checks["fresh-start"]["_delm_revision"], 2);
    }
}
