//! One durable two-worker invocation. The host owns presentation; this module
//! owns worker lifetimes and accepts completions in native event order.
mod completion;
pub(crate) mod questions;
pub(crate) mod state;

use crate::{
    board::{Board, tool_definitions},
    protocol::{Event, HostCommand, StartRequest},
    workers::{RpcClient, text_input, verify_thread_response, worker_config},
    workspace::{self, PreparedWorkspace},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use state::{Journal, RunLock, atomic_json, now};
use std::{collections::HashMap, fs, path::PathBuf, time::Duration};
use tokio::sync::mpsc;

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
    revision_fences: Vec<(u64, u64)>,
    #[serde(default)]
    result_policy: workspace::ResultPolicy,
    waiting: bool,
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

/// The public CLI admits model work only after its separate control command
/// has authenticated. Protocol fixtures already own a bidirectional channel.
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
            saved.status = "paused".into();
            atomic_json(&run_dir.join("run.json"), &saved)?;
            let mut stopped = Event::new(
                "stopped",
                "DeLM stopped before worker startup. No task model turn was started.",
            );
            stopped.run_id = run_dir
                .file_name()
                .map(|id| id.to_string_lossy().into_owned());
            stopped.request_revision = Some(saved.revision);
            stopped.partial_paths = saved.workspace.workers.to_vec();
            let _ = output.send(stopped);
            return Ok(());
        }
        atomic_json(&run_dir.join("run.json"), &saved)?;
    }
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
    let mut rpc = RpcClient::spawn(&saved.request, &run_dir).await?;
    let allowance = if saved.expires == 0 {
        saved.request.seconds
    } else {
        saved.expires.saturating_sub(now())
    };
    if saved.expires == 0 {
        saved.expires = now() + allowance;
    }
    let guard = crate::supervisor::Guard::start_with_paths(
        std::process::id(),
        rpc.pid,
        saved.expires * 1000,
        &rpc.launch_dir,
        std::slice::from_ref(&run_dir),
    )?;
    let result = if *cancel.borrow() {
        Ok(None)
    } else {
        tokio::select! { biased;
            _ = cancel.changed() => Ok(None),
            _ = tokio::time::sleep(Duration::from_secs(allowance)) => Ok(None),
            result = async {rpc.initialize().await?; rpc.verify_account(&saved.request).await?; drive(&mut saved, &mut rpc, &mut board, &mut journal, &mut input, &output).await} => result,
        }
    };
    let threads = saved
        .workers
        .iter()
        .map(|w| (w.thread.clone(), w.turn.clone()))
        .collect::<Vec<_>>();
    let shutdown_started = guard.begin_shutdown();
    let shutdown = rpc.shutdown(&threads).await.and(shutdown_started);
    let supervision = guard.finish();
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
                    "Your update arrived while the workers were stopping. It is saved with the partial work and will be included if you resume"
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
    let mut retention_failed = false;
    let result = match result {
        Ok(Some((winner, candidate))) => {
            let retained = (|| -> Result<PathBuf> {
                ensure!(
                    writers_stopped,
                    "Worker processes did not stop cleanly; both projects are preserved"
                );
                candidate.verify(&saved.workspace.workers[winner])?;
                atomic_json(&run_dir.join("completion.json"), &candidate)?;
                // No owned writer remains before the result and its review are captured.
                workspace::retain_result_with_policy(
                    &saved.workspace,
                    winner,
                    &saved.workers[winner].result_policy,
                )
            })();
            match retained {
                Ok(path) => Ok(Some((winner, path))),
                Err(error) => {
                    retention_failed = true;
                    Err(error.context("Result retention requires recovery"))
                }
            }
        }
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    };
    match result {
        Ok(Some((winner, path))) => {
            saved.status = "complete".into();
            atomic_json(&run_dir.join("run.json"), &saved)?;
            let mut event = Event::new(
                "result",
                saved.workers[winner]
                    .outcome
                    .as_ref()
                    .and_then(|v| v.get("summary"))
                    .and_then(Value::as_str)
                    .unwrap_or("The completed project is ready to open."),
            );
            event.path = Some(path);
            let completion: Value =
                serde_json::from_slice(&fs::read(run_dir.join("completion.json"))?)?;
            event.request_revision = Some(saved.revision);
            event.details = Some(
                json!({"worker":winner+1,"review":run_dir.join("workspace/review"),
                "completion":run_dir.join("completion.json"),"checks":completion["checks"],
                "original_unchanged":true,"previews_stopped":true}),
            );
            event.run_id = run_dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned());
            let _ = output.send(event);
            journal.record("completed", &json!({"worker":winner+1}))?;
        }
        other => {
            saved.status = if writers_stopped && !retention_failed {
                "paused"
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
            if writers_stopped {
                event.partial_paths = saved
                    .workspace
                    .workers
                    .iter()
                    .filter(|path| path.is_dir())
                    .cloned()
                    .collect();
            }
            event.run_id = run_dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned());
            let _ = output.send(event);
            journal.record("paused", &json!({"shutdown_ok":shutdown.is_ok()}))?;
        }
    }
    Ok(())
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
    ensure!(
        !run_dir.starts_with(&request.project),
        "Choose a repository, not the directory containing DeLM's private storage"
    );
    let mut preparing = Event::new(
        "status",
        "Preparing two private workspaces. Your original project will stay unchanged.",
    );
    preparing.run_id = run_dir
        .file_name()
        .map(|id| id.to_string_lossy().into_owned());
    preparing.request_revision = Some(1);
    let _ = output.send(preparing);
    fs::create_dir_all(run_dir.join("attachments"))?;
    request.attachments = crate::inputs::capture(&request.attachments, &run_dir)?;
    let project = request.project.clone();
    let directory = run_dir.clone();
    let mut revision = 1;
    let mut prepare = tokio::task::spawn_blocking(move || {
        workspace::prepare(&project, &directory, crate::config::MAX_REPO_SIZE_BYTES)
    });
    let workspace = loop {
        tokio::select! {
            result = &mut prepare => break result??,
            command = input.recv() => match command {
                Some(HostCommand::Stop) | None => {
                    // Capture is bounded and has no model work or source writes.
                    // Await it before returning so preparation cannot become an orphan.
                    let _ = prepare.await;
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
    let saved = Saved {
        request,
        workspace,
        workers: Default::default(),
        revision,
        expires: 0,
        status: "prepared".into(),
    };
    atomic_json(&run_dir.join("run.json"), &saved)?;
    Ok((saved, lock))
}

async fn start_turn(
    rpc: &RpcClient,
    worker: &mut Worker,
    revision: u64,
    text: &str,
    attachments: &[Value],
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
    worker.revision_fences = vec![(0, revision)];
    worker.outcome = None;
    worker.waiting = false;
    worker.blocked = false;
    Ok(())
}

async fn update_worker(
    rpc: &RpcClient,
    worker: &mut Worker,
    revision: u64,
    text: &str,
    attachments: &[Value],
) -> Result<()> {
    let message = format!("User update, revision {revision}: {text}");
    let mut input = vec![text_input(&message)];
    input.extend_from_slice(attachments);
    let Some(turn) = worker.turn.clone() else {
        return start_turn(rpc, worker, revision, &message, attachments).await;
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
    worker.turn = None;
    start_turn(rpc, worker, revision, &message, attachments).await
}

async fn drive(
    saved: &mut Saved,
    rpc: &mut RpcClient,
    board: &mut Board,
    journal: &mut Journal,
    input: &mut mpsc::Receiver<HostCommand>,
    output: &mpsc::UnboundedSender<Event>,
) -> Result<Option<(usize, completion::Completion)>> {
    for index in 0..2 {
        let config = worker_config(
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
        let profile = format!("delm_worker_{}", index + 1);
        let role = if index == 0 {
            include_str!("../../plugin/worker-1.md")
        } else {
            include_str!("../../plugin/worker-2.md")
        };
        let instructions = format!(
            "{}\n\n{}\n\nThe original project path is {}. Interpret references beneath that path as the corresponding relative paths in your private working directory; never open the original path.",
            include_str!("../../plugin/worker.md"),
            role,
            serde_json::to_string(&saved.workspace.original)?
        );
        let result = if saved.workers[index].thread.is_empty() {
            rpc.request("thread/start", json!({"cwd":saved.workspace.workers[index],"model":saved.request.model,"modelProvider":saved.request.model_provider,"serviceTier":saved.request.service_tier,"permissions":profile,"config":config,"developerInstructions":instructions,"dynamicTools":tool_definitions(),"ephemeral":false})).await?
        } else {
            rpc.request("thread/resume", json!({"threadId":saved.workers[index].thread,"cwd":saved.workspace.workers[index],"model":saved.request.model,"modelProvider":saved.request.model_provider,"serviceTier":saved.request.service_tier,"permissions":profile,"config":config})).await?
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
        atomic_json(&saved.workspace.run_dir.join("run.json"), saved)?;
    }
    if saved.expires == 0 {
        saved.expires = now() + saved.request.seconds;
    }
    ensure!(saved.expires > now(), "The execution allowance has expired");
    saved.status = "running".into();
    let task = format!(
        "User request (revision {}):\n{}\n\nRelevant context supplied by the parent conversation:\n{}\n\nYour project is the private working directory. Start useful work now.",
        saved.revision, saved.request.task, saved.request.context
    );
    for worker in &mut saved.workers {
        start_turn(
            rpc,
            worker,
            saved.revision,
            &task,
            &saved.request.attachments,
        )
        .await?;
    }
    atomic_json(&saved.workspace.run_dir.join("run.json"), saved)?;
    let mut started = Event::new(
        "started",
        "Two DeLM workers are running and sharing progress.",
    );
    started.request_revision = Some(saved.revision);
    let _ = output.send(started);
    let mut questions = questions::Questions::default();
    let mut pending_candidate: Option<(usize, completion::Completion)> = None;
    loop {
        let remaining = saved.expires.saturating_sub(now());
        if remaining == 0 {
            return Ok(None);
        }
        let mut persist = true;
        tokio::select! { biased;
            command = input.recv() => match command {
                None | Some(HostCommand::Stop) => return Ok(None),
                Some(HostCommand::Message{text, attachments}) => {
                    pending_candidate = None;
                    saved.revision += 1; board.set_revision(saved.revision)?;
                    let mut accepted = Event::new("status", "Applying your update to both workers.");
                    accepted.request_revision = Some(saved.revision);
                    let _ = output.send(accepted);
                    saved.request.task.push_str(&format!("\n\nUser update:\n{text}"));
                    journal.record("user_update", &json!({"revision":saved.revision,"text":text}))?;
                    saved.request.attachments.extend(attachments.clone());
                    for worker in &mut saved.workers {
                        worker.outcome = None;
                        update_worker(rpc,worker,saved.revision,&text,&attachments).await?;
                    }
                }
                Some(HostCommand::Answer { id, answers }) => {
                    let pending = match questions.take(&id, &answers) {
                        Ok(pending) => pending,
                        Err(error) => {
                            let mut event = Event::new("answer_rejected", error.to_string()); event.id = Some(id);
                            let _ = output.send(event);
                            if pending_candidate.is_some() {
                                let mut ready = Event::new("ready", "The task is complete; the question had already ended.");
                                ready.request_revision = Some(saved.revision); let _ = output.send(ready);
                            }
                            continue;
                        }
                    };
                    if saved.workers[pending.worker].turn.as_deref() != Some(&pending.turn) {
                        let mut event = Event::new("answer_rejected", "This question belonged to a completed turn; your answer was not applied.");
                        event.id = Some(id); let _ = output.send(event);
                        if pending_candidate.is_some() {
                            let mut ready = Event::new("ready", "The task is complete; the question had already ended.");
                            ready.request_revision = Some(saved.revision); let _ = output.send(ready);
                        }
                        continue;
                    }
                    let response = answers.iter().map(|(key, values)| (key.clone(), json!({"answers":values}))).collect::<serde_json::Map<_,_>>();
                    rpc.respond(pending.native_id, json!({"answers":response})).await?;
                    let _ = output.send(questions::resolved(&id));
                    pending_candidate = None;
                    saved.revision += 1;
                    board.set_revision(saved.revision)?;
                    let text = format!("User answered these questions: {}\nAnswers: {}", pending.items, serde_json::to_string(&answers)?);
                    saved.request.task.push_str(&format!("\n\n{text}"));
                    journal.record("user_answer", &json!({"id":id,"revision":saved.revision,"questions":pending.items,"answers":answers}))?;
                    let mut event = Event::new("answer_applied", "Applying your answer to both workers.");
                    event.id = Some(id);
                    event.request_revision = Some(saved.revision);
                    let _ = output.send(event);
                    for worker in &mut saved.workers {
                        worker.outcome = None;
                        update_worker(rpc,worker,saved.revision,&text,&[]).await?;
                    }
                }
                Some(HostCommand::Approval{..}) => {},
                Some(HostCommand::AcceptResult{request_revision})
                    if request_revision == saved.revision
                        && pending_candidate.as_ref().is_some_and(|(_,candidate)|candidate.revision == request_revision) => {
                    return Ok(pending_candidate.take());
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
                    continue;
                }
                let identity = params.get("threadId").and_then(Value::as_str).and_then(|id|saved.workers.iter().position(|w|w.thread == id));
                let Some(index) = identity else { if let Some(id) = message.get("id") { rpc.reject(id.clone(),"Unbound worker thread").await?; } continue; };
                match method {
                    "item/tool/call" => {
                        let id = message.get("id").context("Tool request identity missing")?.clone();
                        if params.get("turnId").and_then(Value::as_str) != saved.workers[index].turn.as_deref() {
                            rpc.reject(id,"This tool call belongs to a retired turn").await?; continue;
                        }
                        let tool = params.get("tool").and_then(Value::as_str).unwrap_or("");
                        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                        let checked = if tool == "delm_complete" {completion::validate_checks(&args,&saved.workers[index].checks,saved.revision).map(|_|())} else {Ok(())};
                        let result = checked.and_then(|()|board.call(index+1,tool,args.clone()));
                        let success = result.is_ok();
                        let mut body = match result { Ok(value)=>value,Err(error)=>json!({"error":error.to_string(),"board":board.view()?}) };
                        body["recent_commands"] = recent_commands(&saved.workers[index], saved.revision);
                        if success && tool == "delm_complete" { saved.workers[index].outcome = Some(args); }
                        if success && matches!(tool,"delm_publish"|"delm_status")
                            && let Some(summary) = params.pointer("/arguments/summary").and_then(Value::as_str) {
                            let summary: String = summary.chars().take(240).collect();
                            let _ = output.send(Event::new("status",format!("Worker {}: {summary}",index+1)));
                        }
                        rpc.respond(id,json!({"contentItems":[{"type":"inputText","text":serde_json::to_string(&body)?}],"success":success})).await?;
                        if success && matches!(tool,"delm_publish"|"delm_task_finish") {
                            let other = 1-index;
                            let dependency = saved.workers[other].outcome.as_ref().and_then(|value|value.get("dependency")).and_then(Value::as_str).unwrap_or("");
                            let owner_matches = board.dependency_owner(dependency).ok().flatten() == Some(index+1);
                            let task_matches = dependency.strip_prefix("task:").and_then(|id|id.parse::<u64>().ok()).is_some_and(|id|Some(id) == params.pointer("/arguments/task_id").and_then(Value::as_u64));
                            if saved.workers[other].waiting && saved.workers[other].turn.is_none() && (owner_matches || task_matches) {
                                start_turn(rpc,&mut saved.workers[other],saved.revision,"Your peer published new progress. Read the board and continue the named dependent work if it is now available.",&[]).await?;
                            }
                        }
                    }
                    "item/started" => {
                        if let Some(item) = params.get("item")
                            && item["type"].as_str() == Some("commandExecution")
                            && params["turnId"].as_str() == saved.workers[index].turn.as_deref()
                            && let Some(id) = item["id"].as_str() {
                            let _ = id;
                            record_command(&mut saved.workers[index], item, true, message["_delm_received_sequence"].as_u64());
                        }
                    }
                    "item/completed" => {
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
                        for event in questions.retire_turn(index, turn["id"].as_str().unwrap_or("")) { let _ = output.send(event); }
                        saved.workers[index].turn = None;
                        let status = turn.get("status").and_then(Value::as_str).unwrap_or("");
                        if status != "completed" { saved.workers[index].blocked = true; let _=output.send(Event::new("notice",format!("Worker {} stopped with status {status}. Its work is preserved.",index+1))); }
                        else if saved.workers[index].revision == saved.revision {
                            let outcome = saved.workers[index].outcome.clone();
                            match outcome.as_ref().and_then(|o|o.get("outcome")).and_then(Value::as_str) {
                                Some("complete") => {
                                    // User commands already queued take precedence over acceptance.
                                    if input.is_empty() && pending_candidate.is_none() {
                                        journal.record("candidate",&json!({"worker":index+1,"revision":saved.revision,"declaration":outcome,"commands":saved.workers[index].checks}))?;
                                        atomic_json(&saved.workspace.run_dir.join("run.json"),saved)?;
                                        let candidate = completion::Completion::capture(&saved.workspace.workers[index],outcome.as_ref().context("Completion declaration missing")?,&saved.workers[index].checks,saved.revision,&saved.workers[index].result_policy)?;
                                        let mut ready = Event::new("ready","The task is complete. Stopping the workers before opening the result.");
                                        ready.request_revision = Some(saved.revision);
                                        let _ = output.send(ready);
                                        pending_candidate = Some((index,candidate));
                                    }
                                }
                                Some("partial") => {
                                    let summary = outcome.as_ref().and_then(|o|o.get("summary")).and_then(Value::as_str).unwrap_or("");
                                    start_turn(rpc,&mut saved.workers[index],saved.revision,&format!("Continue the concrete unfinished requirement you identified: {summary}. Read relevant peer progress and finish the whole request."),&[]).await?;
                                }
                                Some("waiting") => saved.workers[index].waiting = true,
                                Some("blocked") => {
                                    saved.workers[index].blocked = true;
                                    let reason=outcome.as_ref().and_then(|value|value.get("summary")).and_then(Value::as_str).unwrap_or("Your input is needed.");
                                    let _=output.send(Event::new("notice",format!("Worker {}: {reason}",index+1)));
                                }
                                _ if !saved.workers[index].repaired => {
                                    saved.workers[index].repaired = true;
                                    start_turn(rpc,&mut saved.workers[index],saved.revision,"State the whole-request outcome with delm_complete. If a concrete requested requirement remains, finish it. If the result is ready, declare complete with the checks already performed; do not start another improvement pass.",&[]).await?;
                                }
                                _ => saved.workers[index].blocked = true,
                            }
                        }
                        if pending_candidate.is_none() && saved.workers.iter().all(|w|w.turn.is_none()) {
                            let repair_wait = saved.workers.iter().any(|w|w.waiting && !w.wait_repaired);
                            if repair_wait {
                                for worker in &mut saved.workers {
                                    if worker.waiting && !worker.wait_repaired {
                                        worker.wait_repaired = true;
                                        start_turn(rpc,worker,saved.revision,"Your named dependency has no active producer. Read the board and resolve the dependency through useful work you can do now. If it requires user input, declare blocked with the concrete reason. Do not repeat the same wait.",&[]).await?;
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
                    "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                        // Native acceptance can bypass the sandbox. Never grant it
                        // during a run whose original repository must be immutable.
                        let id = message.get("id").context("Approval identity missing")?.clone();
                        rpc.respond(id,json!({"decision":"decline"})).await?;
                        let _=output.send(Event::new("notice","A worker requested access outside its private permissions. The request was declined; the worker can continue within its project."));
                    }
                    _ => { if let Some(id) = message.get("id") { rpc.reject(id.clone(),"This native capability is unavailable during DeLM").await?; } }
                }
            },
            _ = tokio::time::sleep(Duration::from_secs(remaining)) => return Ok(None),
        }
        if persist {
            atomic_json(&saved.workspace.run_dir.join("run.json"), saved)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
