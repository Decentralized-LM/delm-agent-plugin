//! Host-specific lifecycle over the shared board, evidence, and delivery engines.
//! Only the authenticated native module may send control operations. MCP calls
//! consume one-use tickets reserved by that module before native permission UI.
use crate::{
    board::Board,
    completion::{Completion, validate_evidence},
    evidence::{CommandCompletion, CommandEvidence, FilesystemScope, NativeHost},
    run::state::{self, Journal, RunLock, atomic_json},
    services::Services,
    supervisor::{ProcessIdentity, ensure_workspace_quiet},
    workspace::{self, PreparedWorkspace, ResultPolicy},
};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const MAX_PENDING: usize = 128;

#[derive(Default, Serialize)]
struct Worker {
    agent_id: Option<String>,
    turn_id: Option<String>,
    revision: u64,
    outcome: Option<Value>,
    checks: HashMap<String, CommandEvidence>,
    #[serde(skip)]
    commands: HashMap<String, StartedCommand>,
    #[serde(skip)]
    retired_turns: HashSet<String>,
    #[serde(skip)]
    observed_calls: HashSet<String>,
    waiting: bool,
    resume_pending: bool,
    blocked: bool,
    repaired: bool,
    wait_repaired: bool,
    result_policy: ResultPolicy,
}

struct StartedCommand {
    turn_id: String,
    revision: u64,
    sequence: u64,
    command: String,
    cwd: PathBuf,
}

struct Ticket {
    worker: usize,
    turn_id: String,
    revision: u64,
    tool: String,
    arguments: Value,
}

pub(super) struct Controller {
    _lock: Option<RunLock>,
    workspace: PreparedWorkspace,
    session_id: String,
    task: String,
    revision: u64,
    sequence: u64,
    native_host: ProcessIdentity,
    runtime: ProcessIdentity,
    workers: [Worker; 2],
    board: Board,
    services: Services,
    tickets: HashMap<String, Ticket>,
    candidate: Option<(usize, Completion)>,
    journal: Journal,
    stop_requested: bool,
    finished: bool,
    status: String,
    token_digest: Option<String>,
}

impl Controller {
    pub(super) fn new(
        project: &Path,
        session_id: String,
        task: String,
        native_host: ProcessIdentity,
    ) -> Result<Self> {
        ensure!(
            !session_id.trim().is_empty() && session_id.len() <= 256,
            "Native session identity is missing"
        );
        ensure!(!task.trim().is_empty(), "Enter a task after /delm:run");
        ensure!(
            native_host.is_running()?,
            "Native Claude host is not running"
        );
        let runtime = ProcessIdentity::capture(std::process::id())?;
        ensure!(
            runtime.uid == native_host.uid,
            "Native host belongs to another user"
        );
        let project = project
            .canonicalize()
            .context("Selected project is unavailable")?;
        state::check_storage_boundary(&project)?;
        let lock = RunLock::acquire(&project)?;
        let run_dir = state::create_run()?;
        atomic_json(
            &run_dir.join("claude.json"),
            &json!({"version":1,"host":"claude","project":project,
            "session_id":session_id,"task":task,"runtime":runtime,"native_host":native_host,"status":"preparing","finished":false}),
        )?;
        let workspace =
            match workspace::prepare(&project, &run_dir, crate::config::MAX_REPO_SIZE_BYTES) {
                Ok(workspace) => workspace,
                Err(error) => {
                    // The preparation guard normally removes its captures. If any
                    // directory remains, keep admission closed instead of inferring
                    // ownership from its name or deleting uncertain data.
                    let captures = run_dir.join("workspace");
                    let clean = !captures.try_exists()?
                        || fs::read_dir(&captures)?.all(|entry| {
                            entry.is_ok_and(|entry| {
                                entry.file_type().is_ok_and(|kind| kind.is_file())
                            })
                        });
                    atomic_json(
                        &run_dir.join("claude.json"),
                        &json!({"version":1,"host":"claude","project":project,
                    "session_id":session_id,"task":task,"runtime":runtime,"native_host":native_host,
                    "status":"preparation_failed","finished":clean,"reason":error.to_string()}),
                    )?;
                    return Err(error);
                }
            };
        Self::from_workspace(lock, workspace, session_id, task, native_host, runtime)
    }

    fn from_workspace(
        lock: RunLock,
        workspace: PreparedWorkspace,
        session_id: String,
        task: String,
        native_host: ProcessIdentity,
        runtime: ProcessIdentity,
    ) -> Result<Self> {
        // Keep the project lock outside fallible construction, including failure
        // recovery. No native identity or launch path has been published yet.
        let saved = serde_json::to_value(&workspace)?;
        let failure_record = json!({"version":1,"host":"claude","session_id":session_id,"task":task,
            "project":workspace.original,"workspace":saved,"runtime":runtime,"native_host":native_host});
        match Self::assemble(workspace, session_id, task, native_host, runtime) {
            Ok(mut controller) => {
                controller._lock = Some(lock);
                Ok(controller)
            }
            Err(error) => {
                let prepared: PreparedWorkspace = serde_json::from_value(saved)?;
                let cleanup = cleanup_before_handoff(&prepared, runtime, native_host);
                let mut record = failure_record;
                record["reason"] = json!(format!("{error:#}"));
                record["status"] = json!(if cleanup.is_ok() {
                    "startup_failed"
                } else {
                    "recovery_required"
                });
                record["finished"] = json!(cleanup.is_ok());
                match &cleanup {
                    Ok(recovery) => record["recovery"] = serde_json::to_value(recovery)?,
                    Err(failure) => record["cleanup_error"] = json!(format!("{failure:#}")),
                }
                atomic_json(&prepared.run_dir.join("claude.json"), &record)?;
                match cleanup {
                    Ok(_) => Err(error
                        .context("Native launch did not occur; temporary workspaces were removed")),
                    Err(failure) => Err(error.context(format!(
                        "Native launch did not occur; workspace recovery is required: {failure:#}"
                    ))),
                }
            }
        }
    }

    fn assemble(
        workspace: PreparedWorkspace,
        session_id: String,
        task: String,
        native_host: ProcessIdentity,
        runtime: ProcessIdentity,
    ) -> Result<Self> {
        let mut board = Board::open(
            &workspace.run_dir,
            &workspace.baseline,
            workspace.workers.clone(),
        )?;
        // The adapter grants own-workspace board access under the dedicated
        // DeLM MCP tool permission. This is separate from native Read/Edit
        // rules, never authority supplied by model arguments.
        for slot in 1..=2 {
            board.set_worker_scopes(slot, Vec::new())?;
        }
        let journal = Journal::open(&workspace.run_dir)?;
        let python_policy = ResultPolicy {
            native_python_runtime: true,
            readonly_runtime_roots: Vec::new(),
            denied_roots: vec![
                workspace.original.clone(),
                workspace
                    .run_dir
                    .parent()
                    .context("Missing private runs directory")?
                    .to_path_buf(),
            ],
        };
        let workers = std::array::from_fn(|_| Worker {
            result_policy: python_policy.clone(),
            ..Worker::default()
        });
        let mut result = Self {
            _lock: None,
            workspace,
            session_id,
            task,
            revision: 1,
            sequence: 0,
            native_host,
            runtime,
            workers,
            board,
            services: Services::default(),
            tickets: HashMap::new(),
            candidate: None,
            journal,
            stop_requested: false,
            finished: false,
            status: "prepared".into(),
            token_digest: None,
        };
        result.journal.record("claude_prepared", &result.ready())?;
        result.persist()?;
        Ok(result)
    }

    /// Called by the transport only before publishing the ready response. It
    /// must never be used to infer that an already published launch was unused.
    pub(super) fn abort_before_handoff(&mut self, reason: &str) -> Result<Value> {
        ensure!(
            self.workers.iter().all(|worker| worker.agent_id.is_none())
                && self.status == "prepared"
                && !self.finished,
            "Native handoff rollback is no longer available"
        );
        self.stop_requested = true;
        self.tickets.clear();
        self.status = "stopping".into();
        self.journal
            .record("claude_handoff_aborted", &json!({"reason":reason}))?;
        let cleanup = cleanup_before_handoff(&self.workspace, self.runtime, self.native_host);
        match cleanup {
            Ok(recovery) => {
                self.status = "startup_failed".into();
                self.finished = true;
                let result = json!({"status":self.status,"reason":reason,"recovery":recovery});
                self.journal.record("claude_startup_cleanup", &result)?;
                self.persist()?;
                Ok(result)
            }
            Err(error) => {
                self.status = "recovery_required".into();
                self.persist()?;
                Err(error.context(
                    "Native handoff was not published; preserve the workspaces for recovery",
                ))
            }
        }
    }

    pub(super) fn ready(&self) -> Value {
        json!({"run_id":self.workspace.run_dir.file_name().unwrap_or_default().to_string_lossy(),
            "run_dir":self.workspace.run_dir,"session_id":self.session_id,"revision":self.revision,
            "workers":[{"slot":1,"cwd":self.workspace.workers[0]},{"slot":2,"cwd":self.workspace.workers[1]}],
            "task":self.task,"status":self.status})
    }

    pub(super) fn finished(&self) -> bool {
        self.finished
    }

    pub(super) fn handle(&mut self, request: Value) -> Result<Value> {
        ensure!(
            request.is_object(),
            "Native control request must be an object"
        );
        ensure!(
            !self.finished || request["op"] == "status",
            "This DeLM run is finished"
        );
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("Native event sequence overflow")?;
        let mut result = self.dispatch(&request);
        if let Ok(body) = &mut result {
            let mut index = 0;
            stamp_actions(body, self.sequence, &mut index);
        }
        // Persist even an error that burned a one-use ticket or interrupted a
        // partially completed lifecycle transition. Never persist ticket secrets.
        self.persist()?;
        result
    }

    fn dispatch(&mut self, request: &Value) -> Result<Value> {
        match text(request, "op", 80)? {
            "transport" => {
                ensure!(
                    self.token_digest.is_none()
                        && self.workers.iter().all(|w| w.agent_id.is_none()),
                    "Transport identity is already registered"
                );
                let digest = text(request, "token_digest", 64)?;
                ensure!(
                    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
                    "Invalid transport token digest"
                );
                self.token_digest = Some(digest.to_ascii_lowercase());
                Ok(json!({"registered":true}))
            }
            "bind" => self.bind(request),
            "step" => self.step(request),
            "configure_scopes" => self.configure_scopes(request),
            "reserve" => self.reserve(request),
            "revoke" => {
                Ok(json!({"revoked":self.tickets.remove(text(request,"ticket",128)?).is_some()}))
            }
            "consume" => self.consume(request),
            "command_start" => self.command_start(request),
            "command_end" => self.command_end(request),
            "turn_end" => self.turn_end(request),
            "update" => self.update(request),
            "cancel" => {
                self.stop_requested = true;
                self.candidate = None;
                self.tickets.clear();
                self.status = "stopping".into();
                self.journal.record(
                    "claude_cancelled",
                    &json!({"reason":text(request,"reason",4096)?}),
                )?;
                Ok(
                    json!({"actions":[{"type":"stop","reason":"cancelled","agents":self.bound_agents()}]}),
                )
            }
            "settle" => self.settle(request),
            "status" => Ok(
                json!({"run":self.ready(),"board":self.board.view()?,"agents":self.workers,
                "candidate":self.candidate.as_ref().map(|(i,_)|self.workers[*i].agent_id.clone()),"finished":self.finished}),
            ),
            op => bail!("Unknown native control operation: {op}"),
        }
    }

    fn bind(&mut self, r: &Value) -> Result<Value> {
        ensure!(!self.stop_requested, "Run is stopping");
        let slot = r["slot"]
            .as_u64()
            .filter(|slot| (1..=2).contains(slot))
            .context("Worker slot must be 1 or 2")? as usize
            - 1;
        let agent = text(r, "agent_id", 256)?;
        ensure!(
            !self
                .workers
                .iter()
                .enumerate()
                .any(|(i, w)| i != slot && w.agent_id.as_deref() == Some(agent)),
            "Native agent is already bound to the other worker"
        );
        ensure!(
            self.workers[slot]
                .agent_id
                .as_deref()
                .is_none_or(|bound| bound == agent),
            "Worker slot is already bound"
        );
        let newly_bound = self.workers[slot].agent_id.is_none();
        self.workers[slot].agent_id = Some(agent.into());
        self.status = "running".into();
        let mut actions = Vec::new();
        // Both native forks inherited the original launch contract. A user
        // update may arrive before either spawn result can bind its identity.
        // Deliver the cumulative request once, without relabeling that old
        // inherited context as an acknowledgment of the current revision.
        if newly_bound && self.revision > 1 {
            self.workers[slot].resume_pending = true;
            actions.push(json!({"type":"context","agent_id":agent,"revision":self.revision,"message":self.task}));
        }
        Ok(
            json!({"slot":slot+1,"revision":self.revision,"cwd":self.workspace.workers[slot],"actions":actions}),
        )
    }

    fn worker(&self, r: &Value) -> Result<usize> {
        let agent = text(r, "agent_id", 256)?;
        self.workers
            .iter()
            .position(|w| w.agent_id.as_deref() == Some(agent))
            .context("Unknown native worker identity")
    }

    fn active(&self, r: &Value) -> Result<usize> {
        ensure!(
            !self.stop_requested && self.candidate.is_none(),
            "Run is awaiting native shutdown"
        );
        let i = self.worker(r)?;
        let turn = text(r, "turn_id", 256)?;
        ensure!(
            self.workers[i].turn_id.as_deref() == Some(turn),
            "Native turn is not active"
        );
        ensure!(
            self.workers[i].revision == self.revision,
            "Worker must acknowledge the current task revision"
        );
        Ok(i)
    }

    fn step(&mut self, r: &Value) -> Result<Value> {
        ensure!(
            !self.stop_requested && self.candidate.is_none(),
            "Run is awaiting native shutdown"
        );
        let i = self.worker(r)?;
        let turn = text(r, "turn_id", 256)?;
        ensure!(
            r["revision"].as_u64() == Some(self.revision),
            "Native step must acknowledge the current task revision"
        );
        ensure!(
            !self.workers[i].retired_turns.contains(turn),
            "A retired native turn cannot reactivate"
        );
        if let Some(active) = self.workers[i].turn_id.as_deref() {
            ensure!(active == turn, "Previous native turn is still active");
        }
        if self.workers[i].revision != self.revision || self.workers[i].turn_id.is_none() {
            self.workers[i].outcome = None;
            self.workers[i].waiting = false;
            self.workers[i].blocked = false;
        }
        self.workers[i].resume_pending = false;
        self.workers[i].turn_id = Some(turn.into());
        self.workers[i].revision = self.revision;
        Ok(json!({"revision":self.revision,"task":self.task}))
    }

    fn configure_scopes(&mut self, r: &Value) -> Result<Value> {
        let i = self.worker(r)?;
        let scopes: Vec<FilesystemScope> = serde_json::from_value(r["scopes"].clone())?;
        self.board.set_worker_scopes(i + 1, scopes)?;
        if let Some(policy) = r.get("result_policy") {
            let mut policy: ResultPolicy = serde_json::from_value(policy.clone())?;
            // Host customization cannot allow interpreter routes through the
            // original checkout, this run, or another private DeLM run.
            policy
                .denied_roots
                .extend(self.workers[i].result_policy.denied_roots.iter().cloned());
            policy.denied_roots.sort();
            policy.denied_roots.dedup();
            self.workers[i].result_policy = policy;
        }
        Ok(json!({"configured":true}))
    }

    fn reserve(&mut self, r: &Value) -> Result<Value> {
        let i = self.active(r)?;
        let call = text(r, "call_id", 256)?;
        ensure!(
            self.tickets.len() < MAX_PENDING,
            "Too many pending native tool approvals"
        );
        let tool = text(r, "tool", 128)?;
        ensure!(
            crate::board::tool_definitions()
                .iter()
                .chain(crate::services::tool_definitions().iter())
                .any(|v| v["name"] == tool),
            "Unknown DeLM coordination tool"
        );
        let arguments = r["arguments"].clone();
        ensure!(
            arguments.is_object() && serde_json::to_vec(&arguments)?.len() <= 128 * 1024,
            "Invalid coordination arguments"
        );
        ensure!(
            self.workers[i].observed_calls.insert(call.into()),
            "Native call identity was already reserved"
        );
        let ticket = uuid::Uuid::new_v4().to_string();
        self.tickets.insert(
            ticket.clone(),
            Ticket {
                worker: i,
                turn_id: text(r, "turn_id", 256)?.into(),
                revision: self.revision,
                tool: tool.into(),
                arguments,
            },
        );
        Ok(json!({"ticket":ticket}))
    }

    fn consume(&mut self, r: &Value) -> Result<Value> {
        let ticket = self
            .tickets
            .remove(text(r, "ticket", 128)?)
            .context("Unknown or already consumed native ticket")?;
        let i = ticket.worker;
        ensure!(
            !self.stop_requested && self.candidate.is_none(),
            "Run is awaiting native shutdown"
        );
        ensure!(
            ticket.revision == self.revision
                && self.workers[i].revision == self.revision
                && self.workers[i].turn_id.as_deref() == Some(&ticket.turn_id),
            "Ticket belongs to an obsolete native turn or task revision"
        );
        ensure!(
            r["tool"] == ticket.tool && r["arguments"] == ticket.arguments,
            "Native ticket tool or arguments changed"
        );
        let tool = ticket.tool.as_str();
        let args = ticket.arguments;
        if tool == "delm_complete" {
            validate_evidence(&args, &self.workers[i].checks, self.revision)?;
        }
        let mut body = match tool {
            "delm_check_begin" => self
                .board
                .begin_check(i + 1, args.clone(), || self.sequence)?,
            "delm_check_finish" => self.board.finish_check_with_evidence(
                i + 1,
                args.clone(),
                &self.workers[i].checks,
            )?,
            "delm_service" => {
                self.services
                    .call(i + 1, args.clone(), self.native_host.pid, &self.board)?
            }
            _ => self.board.call(i + 1, tool, args.clone())?,
        };
        if tool == "delm_complete" {
            self.workers[i].outcome = Some(args.clone());
        }
        self.journal.record("claude_coordination",&json!({"worker":i+1,"revision":self.revision,"tool":tool,"arguments":args,"response":body}))?;
        body["recent_commands"] = json!(
            self.workers[i]
                .checks
                .values()
                .filter(|v| v.revision == self.revision)
                .map(|v| json!({"id":v.id,"command":v.command,"completion":v.completion}))
                .collect::<Vec<_>>()
        );
        let mut actions = Vec::new();
        if matches!(
            tool,
            "delm_publish"
                | "delm_task_finish"
                | "delm_task_release"
                | "delm_task_split"
                | "delm_task_create"
        ) {
            self.wake_waiting(&mut actions,"Shared work changed. Read the board, take ready work, and reuse the new contribution.");
        }
        body["actions"] = json!(actions);
        Ok(body)
    }

    fn command_start(&mut self, r: &Value) -> Result<Value> {
        let i = self.active(r)?;
        let id = text(r, "call_id", 256)?.to_owned();
        ensure!(
            !self.workers[i].commands.contains_key(&id)
                && !self.workers[i].checks.contains_key(&id),
            "Native command identity was already observed"
        );
        let cwd = PathBuf::from(text(r, "cwd", 16384)?);
        ensure!(
            cwd.is_absolute(),
            "Native execution cwd must be observed and absolute"
        );
        self.workers[i].commands.insert(
            id,
            StartedCommand {
                turn_id: text(r, "turn_id", 256)?.into(),
                revision: self.revision,
                sequence: self.sequence,
                command: text(r, "command", 128 * 1024)?.into(),
                cwd,
            },
        );
        Ok(json!({"recorded":true}))
    }

    fn command_end(&mut self, r: &Value) -> Result<Value> {
        let i = self.worker(r)?;
        let id = text(r, "call_id", 256)?.to_owned();
        let started = self.workers[i]
            .commands
            .remove(&id)
            .context("Native command start was not observed")?;
        ensure!(
            self.workers[i].turn_id.as_deref() == Some(&started.turn_id),
            "Native command belongs to a retired turn"
        );
        let evidence = CommandEvidence {
            host: NativeHost::Claude,
            id: id.clone(),
            command: started.command,
            cwd: started.cwd,
            revision: started.revision,
            started_sequence: Some(started.sequence),
            completion: CommandCompletion::NativeTool {
                result_ref: text(r, "result_ref", 256)?.into(),
                is_error: boolean(r, "is_error")?,
                interrupted: boolean(r, "interrupted")?,
                background_task_id: match r.get("background_task_id") {
                    Some(Value::Null) | None => None,
                    Some(Value::String(id)) if !id.is_empty() => Some(id.clone()),
                    _ => bail!("Invalid native background task identity"),
                },
                timed_out: boolean(r, "timed_out")?,
            },
        };
        self.journal.record("claude_command_completed", &evidence)?;
        self.workers[i].checks.insert(id, evidence);
        Ok(json!({"recorded":true}))
    }

    fn turn_end(&mut self, r: &Value) -> Result<Value> {
        let i = self.worker(r)?;
        let turn = text(r, "turn_id", 256)?;
        ensure!(
            self.workers[i].turn_id.as_deref() == Some(turn),
            "Native turn completion is stale or duplicated"
        );
        self.workers[i].retired_turns.insert(turn.into());
        self.workers[i].turn_id = None;
        self.workers[i]
            .commands
            .retain(|_, command| command.turn_id != turn);
        self.tickets.retain(|_, ticket| ticket.worker != i);
        let reason = text(r, "reason", 128)?;
        let answer = r["answer"].as_str().unwrap_or("");
        self.journal.record("claude_turn_completed",&json!({"worker":i+1,"turn_id":turn,"reason":reason,"answer":answer,"revision":self.workers[i].revision}))?;
        let mut actions = Vec::new();
        if reason != "completed" {
            self.workers[i].blocked = true;
            self.board.release_worker_claims(i + 1, reason)?;
            self.services.retire_worker(i + 1)?;
            self.wake_waiting(&mut actions,"Your peer stopped. Read the released tasks and continue useful work from its published contributions.");
        } else if !self.stop_requested
            && self.workers[i].revision == self.revision
            && self.candidate.is_none()
        {
            let declaration = self.workers[i].outcome.clone();
            match declaration.as_ref().and_then(|value|value["outcome"].as_str()) {
                Some("complete")=>{
                    ensure!(!answer.trim().is_empty(),"Native completed turn has no final answer");
                    let declaration=declaration.as_ref().unwrap();
                    let shared=self.board.shared_checks(i+1,declaration,self.revision)?;
                    let completion=Completion::capture_with_evidence(&self.workspace.workers[i],declaration,&self.workers[i].checks,self.revision,&self.workers[i].result_policy,shared)?;
                    atomic_json(&self.workspace.run_dir.join("completion.json"),&completion)?;
                    self.candidate=Some((i,completion));
                    self.status="awaiting_shutdown".into();
                    self.tickets.clear();
                    actions.push(json!({"type":"candidate","agent_id":self.workers[i].agent_id,"revision":self.revision}));
                }
                Some("partial")=>actions.push(self.resume(i,"Continue the concrete unfinished requirement in your declaration. Read ownership and reuse peer contributions; do not repeat completed checks.")),
                Some("waiting")=>self.workers[i].waiting=true,
                Some("blocked")=>self.workers[i].blocked=true,
                _ if !self.workers[i].repaired=>{
                    self.workers[i].repaired=true;
                    actions.push(self.resume(i,"State the whole-request outcome with delm_complete, then end your turn. If ready, reference checks already performed; do not start another improvement pass."));
                }
                _=>self.workers[i].blocked=true,
            }
        }
        if self.candidate.is_none()
            && !self.stop_requested
            && self
                .workers
                .iter()
                .all(|w| w.turn_id.is_none() && !w.resume_pending)
        {
            for j in 0..2 {
                if self.workers[j].waiting && !self.workers[j].wait_repaired {
                    self.workers[j].wait_repaired = true;
                    self.workers[j].waiting = false;
                    actions.push(self.resume(j,"Your dependency has no active producer. Resolve available work now, or declare blocked with the concrete input needed."));
                }
            }
            if actions.is_empty() && self.workers.iter().all(|w| w.agent_id.is_some()) {
                self.stop_requested = true;
                self.status = "stopping".into();
                actions.push(json!({"type":"stop","reason":"No active worker can continue","agents":self.bound_agents()}));
            }
        }
        Ok(json!({"actions":actions}))
    }

    fn resume(&mut self, i: usize, message: &str) -> Value {
        self.workers[i].resume_pending = true;
        json!({"type":"resume","agent_id":self.workers[i].agent_id,"revision":self.revision,"message":message})
    }
    fn wake_waiting(&mut self, actions: &mut Vec<Value>, message: &str) {
        if self.stop_requested || self.candidate.is_some() {
            return;
        }
        for i in 0..2 {
            if self.workers[i].waiting && self.workers[i].turn_id.is_none() {
                self.workers[i].waiting = false;
                actions.push(self.resume(i, message));
            }
        }
    }
    fn bound_agents(&self) -> Vec<&str> {
        self.workers
            .iter()
            .filter_map(|w| w.agent_id.as_deref())
            .collect()
    }

    fn update(&mut self, r: &Value) -> Result<Value> {
        ensure!(
            !self.stop_requested,
            "Run is already stopping; restart with the saved update after recovery"
        );
        let update = text(r, "text", 128 * 1024)?;
        self.revision = self
            .revision
            .checked_add(1)
            .context("Task revision overflow")?;
        self.task.push_str(&format!("\n\nUser update:\n{update}"));
        self.board.set_revision(self.revision)?;
        self.candidate = None;
        self.tickets.clear();
        for worker in &mut self.workers {
            worker.outcome = None;
            worker.waiting = false;
            worker.blocked = false;
            worker.repaired = false;
            worker.wait_repaired = false;
        }
        self.status = "running".into();
        self.journal.record(
            "claude_user_update",
            &json!({"revision":self.revision,"text":update}),
        )?;
        let mut actions = Vec::new();
        for i in 0..2 {
            if self.workers[i].agent_id.is_some() {
                if self.workers[i].turn_id.is_some() {
                    // Hold liveness if an old native turn finishes before the
                    // host delivers this update. The next acknowledged step
                    // clears this flag; an append alone is not an acknowledgment.
                    self.workers[i].resume_pending = true;
                    actions.push(json!({"type":"context","agent_id":self.workers[i].agent_id,
                        "revision":self.revision,"message":update}));
                } else {
                    actions.push(self.resume(i, update));
                }
            }
        }
        Ok(json!({"revision":self.revision,"task":self.task,"actions":actions}))
    }

    fn settle(&mut self, r: &Value) -> Result<Value> {
        ensure!(
            self.stop_requested || self.candidate.is_some(),
            "Run has no completed result or stop request"
        );
        ensure!(
            boolean(r, "background_tasks_stopped")?,
            "Native background tasks have not all stopped"
        );
        let agents = r["agents"]
            .as_array()
            .context("Native agent terminal states are required")?;
        let mut seen = HashSet::new();
        for agent in agents {
            let id = text(agent, "id", 256)?;
            ensure!(seen.insert(id), "Duplicate native agent state");
            ensure!(
                self.bound_agents().contains(&id),
                "Unbound native agent in shutdown proof"
            );
            ensure!(
                matches!(
                    text(agent, "status", 128)?,
                    "completed" | "failed" | "killed" | "finished"
                ),
                "A native worker is still active"
            );
        }
        ensure!(
            self.bound_agents().iter().all(|id| seen.contains(id)),
            "A bound worker has no native shutdown acknowledgment"
        );
        self.status = "verifying_shutdown".into();
        self.persist()?;
        let mut quiet = false;
        let result = (|| -> Result<Value> {
            let processes = self.services.process_identities();
            ensure_services_stopped(&processes)?;
            let mut known = processes;
            known.push(self.native_host);
            ensure_workspace_quiet(
                &[self.workspace.run_dir.join("workspace")],
                self.runtime,
                &known,
            )?;
            quiet = true;
            if let Some((i, candidate)) = &self.candidate {
                ensure!(
                    self.workers.iter().all(|worker| worker.agent_id.is_some()),
                    "Both native peers must be bound before result delivery"
                );
                ensure!(
                    !self.stop_requested && candidate.revision == self.revision,
                    "Candidate is obsolete or cancelled"
                );
                candidate.verify(&self.workspace.workers[*i])?;
                let delivery = workspace::deliver_result(
                    &self.workspace,
                    *i,
                    &self.workers[*i].result_policy,
                )?;
                self.status = if !delivery.delivered {
                    "delivery_conflict"
                } else if delivery.verification_required {
                    "delivered"
                } else {
                    "complete"
                }
                .into();
                Ok(
                    json!({"type":"final","status":self.status,"delivery":delivery,"summary":candidate.declaration["summary"],"checks":candidate.checks,"shared_checks":candidate.shared_checks}),
                )
            } else {
                let recovery = workspace::preserve_partial_and_cleanup(&self.workspace)?;
                self.status = "stopped".into();
                Ok(json!({"type":"final","status":self.status,"recovery":recovery}))
            }
        })();
        match result {
            Ok(final_event) => {
                self.finished = true;
                self.tickets.clear();
                self.journal.record("claude_final", &final_event)?;
                Ok(json!({"actions":[final_event]}))
            }
            Err(error) => {
                self.status = "recovery_required".into();
                let reason = format!("{error:#}");
                self.journal.record(
                    "claude_recovery_required",
                    &json!({"reason":reason,"writers_stopped":quiet}),
                )?;
                if quiet {
                    // The delivery engine keeps its transaction journal and
                    // displaced bytes outside the removable worker directories.
                    // Preserve both source deltas before removing their copies.
                    match workspace::preserve_partial_and_cleanup(&self.workspace) {
                        Ok(recovery) => {
                            self.finished = true;
                            self.tickets.clear();
                            let final_event = json!({"type":"final","status":"recovery_required","reason":reason,
                                "recovery":recovery,"delivery_journal":self.workspace.run_dir.join("workspace/delivery/journal.json")});
                            self.journal.record("claude_final", &final_event)?;
                            return Ok(json!({"actions":[final_event]}));
                        }
                        Err(cleanup_error) => return Err(error.context(format!("Result was not confirmed; cleanup also requires recovery: {cleanup_error:#}"))),
                    }
                }
                Err(error
                    .context("Native shutdown is not proven; private workspaces are preserved"))
            }
        }
    }

    fn persist(&self) -> Result<()> {
        atomic_json(
            &self.workspace.run_dir.join("claude.json"),
            &json!({"version":1,"host":"claude","session_id":self.session_id,
            "workspace":self.workspace,"project":self.workspace.original,"token_digest":self.token_digest,"task":self.task,"revision":self.revision,"sequence":self.sequence,"native_host":self.native_host,
            "runtime":self.runtime,"workers":self.workers,"services":self.services.process_identities(),"candidate":self.candidate,"status":self.status,
            "stop_requested":self.stop_requested,"finished":self.finished}),
        )
    }
}

fn cleanup_before_handoff(
    workspace: &PreparedWorkspace,
    runtime: ProcessIdentity,
    native_host: ProcessIdentity,
) -> Result<workspace::RecoveryReport> {
    ensure_workspace_quiet(
        &[workspace.run_dir.join("workspace")],
        runtime,
        &[native_host],
    )?;
    workspace::preserve_partial_and_cleanup(workspace)
}

fn stamp_actions(value: &mut Value, sequence: u64, index: &mut usize) {
    match value {
        Value::Object(object) => {
            if let Some(actions) = object.get_mut("actions").and_then(Value::as_array_mut) {
                for action in actions {
                    if action.is_object() {
                        action["id"] = json!(format!("{sequence}-{}", *index));
                        *index += 1;
                    }
                }
            }
            for (key, child) in object {
                if key != "actions" {
                    stamp_actions(child, sequence, index);
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                stamp_actions(child, sequence, index);
            }
        }
        _ => {}
    }
}

/// An orphaned bridge cannot safely deliver a candidate: the native module
/// first stops exactly its retained agent/background IDs, then this path keeps
/// reviewable source changes and removes the two verified private workspaces.
pub(super) fn recover(run_id: &str, request: &Value) -> Result<Value> {
    recover_at(&state::run_path(run_id)?, request)
}

fn recover_at(run_dir: &Path, request: &Value) -> Result<Value> {
    let path = run_dir.join("claude.json");
    let mut saved = read_state(&path)?;
    ensure!(
        saved["version"] == 1 && saved["host"] == "claude",
        "Unknown native recovery record"
    );
    let runtime: ProcessIdentity = serde_json::from_value(saved["runtime"].clone())?;
    ensure!(
        !runtime.is_running()?,
        "The original bridge is still running; request its native stop first"
    );
    let host: ProcessIdentity = serde_json::from_value(saved["native_host"].clone())?;
    let host_stopped = !host.is_running()?;
    if request
        .get("token")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.is_empty())
    {
        ensure!(
            saved["session_id"].as_str() == Some(text(request, "session_id", 256)?),
            "Recovery belongs to another native session"
        );
        let token = text(request, "token", 256)?;
        let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
        let expected = text(&saved, "token_digest", 64)?;
        ensure!(
            digest.len() == expected.len()
                && digest
                    .bytes()
                    .zip(expected.bytes())
                    .fold(0u8, |v, (a, b)| v | (a ^ b))
                    == 0,
            "Recovery capability does not match this run"
        );
    } else {
        ensure!(
            host_stopped,
            "Tokenless recovery requires the saved native host and bridge to have exited"
        );
    }
    let prepared: PreparedWorkspace = serde_json::from_value(saved["workspace"].clone())
        .context("Repository preparation did not finish; preserve this run for manual recovery")?;
    ensure!(
        prepared.run_dir == run_dir
            && prepared.baseline == run_dir.join("workspace/baseline")
            && prepared.workers
                == [
                    run_dir.join("workspace/worker-1"),
                    run_dir.join("workspace/worker-2")
                ],
        "Recovery workspace paths do not match this run"
    );
    let run_id = run_dir
        .file_name()
        .and_then(|id| id.to_str())
        .context("Invalid recovery run identity")?;
    let _lock = RunLock::acquire_for_recovery(&prepared.original, run_id)?;
    if saved["finished"] == true {
        return Ok(
            json!({"status":saved["status"],"already_finished":true,"recovery":saved["recovery"]}),
        );
    }
    if !host_stopped {
        ensure!(
            boolean(request, "background_tasks_stopped")?,
            "Native background tasks have not stopped"
        );
        let workers = saved["workers"]
            .as_array()
            .context("Missing native worker bindings")?;
        ensure!(workers.len() == 2, "Invalid native worker bindings");
        let bound: HashSet<_> = workers
            .iter()
            .filter_map(|w| w["agent_id"].as_str())
            .collect();
        let mut seen = HashSet::new();
        for agent in request["agents"]
            .as_array()
            .context("Native shutdown acknowledgments are required")?
        {
            let id = text(agent, "id", 256)?;
            ensure!(
                bound.contains(id) && seen.insert(id),
                "Unknown or duplicate native recovery agent"
            );
            ensure!(
                matches!(
                    text(agent, "status", 128)?,
                    "completed" | "failed" | "killed" | "finished"
                ),
                "A native recovery agent is still active"
            );
        }
        ensure!(
            seen == bound,
            "A bound worker has no native shutdown acknowledgment"
        );
    }
    let services: Vec<ProcessIdentity> =
        serde_json::from_value(saved.get("services").cloned().unwrap_or_else(|| json!([])))?;
    ensure_services_stopped(&services)?;
    let mut known = services;
    known.push(host);
    ensure_workspace_quiet(&[run_dir.join("workspace")], runtime, &known)?;
    let recovery = workspace::preserve_partial_and_cleanup(&prepared)?;
    saved["status"] = json!("stopped");
    saved["finished"] = json!(true);
    saved["candidate"] = Value::Null;
    saved["recovery"] = serde_json::to_value(&recovery)?;
    atomic_json(&path, &saved)?;
    Ok(json!({"status":"stopped","recovery":recovery}))
}

fn ensure_services_stopped(processes: &[ProcessIdentity]) -> Result<()> {
    for process in processes {
        ensure!(
            !process.is_running()?,
            "Registered preview PID {} is still running; stop its native task before cleanup",
            process.pid
        );
    }
    Ok(())
}

fn read_state(path: &Path) -> Result<Value> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.len() <= 64 * 1024 * 1024,
        "Unsafe or oversized native run record"
    );
    let mut bytes = Vec::new();
    file.by_ref()
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024 * 1024,
        "Native run record exceeded its bound"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn text<'a>(value: &'a Value, key: &str, maximum: usize) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|v| !v.trim().is_empty() && v.len() <= maximum)
        .with_context(|| format!("Missing or invalid {key}"))
}
fn boolean(value: &Value, key: &str) -> Result<bool> {
    value[key]
        .as_bool()
        .with_context(|| format!("Native {key} must be explicitly reported"))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::evidence::FilesystemAccess;
    use std::fs;

    fn prepared_controller() -> (tempfile::TempDir, Controller) {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("source.txt"), "original\n").unwrap();
        let project = project.canonicalize().unwrap();
        let lock = RunLock::acquire(&project).unwrap();
        let workspace = workspace::prepare(
            &project,
            &temp.path().join(uuid::Uuid::new_v4().to_string()),
            1_000_000,
        )
        .unwrap();
        let runtime = ProcessIdentity::capture(std::process::id()).unwrap();
        let c = Controller::from_workspace(
            lock,
            workspace,
            "session".into(),
            "Make a useful change".into(),
            runtime,
            runtime,
        )
        .unwrap();
        (temp, c)
    }
    fn controller() -> (tempfile::TempDir, Controller) {
        let (temp, mut c) = prepared_controller();
        for slot in 1..=2 {
            c.handle(json!({"op":"bind","slot":slot,"agent_id":format!("agent-{slot}")}))
                .unwrap();
            c.handle(json!({"op":"configure_scopes","agent_id":format!("agent-{slot}"),"scopes":[FilesystemScope {path:c.workspace.workers[slot-1].clone(),access:FilesystemAccess::Write}]})).unwrap();
            c.handle(json!({"op":"step","agent_id":format!("agent-{slot}"),"turn_id":format!("turn-{slot}"),"revision":1})).unwrap();
        }
        (temp, c)
    }
    fn reserve(c: &mut Controller, slot: usize, call: &str, tool: &str, args: Value) -> String {
        c.handle(json!({"op":"reserve","agent_id":format!("agent-{slot}"),"turn_id":format!("turn-{slot}"),"call_id":call,"tool":tool,"arguments":args})).unwrap()["ticket"].as_str().unwrap().into()
    }
    fn tool(c: &mut Controller, slot: usize, call: &str, name: &str, args: Value) -> Value {
        let ticket = reserve(c, slot, call, name, args.clone());
        c.handle(json!({"op":"consume","ticket":ticket,"tool":name,"arguments":args}))
            .unwrap()
    }
    fn complete(c: &mut Controller) {
        fs::write(c.workspace.workers[0].join("source.txt"), "finished\n").unwrap();
        tool(
            c,
            1,
            "complete",
            "delm_complete",
            json!({"idempotency_key":"complete","expected_revision":1,"outcome":"complete","summary":"Implemented","checks":[]}),
        );
        let result=c.handle(json!({"op":"turn_end","agent_id":"agent-1","turn_id":"turn-1","reason":"completed","answer":"Implemented the requested change."})).unwrap();
        assert_eq!(result["actions"][0]["type"], "candidate");
    }
    fn settle(c: &mut Controller) -> Result<Value> {
        c.handle(json!({"op":"settle","agents":[{"id":"agent-1","status":"completed"},{"id":"agent-2","status":"killed"}],"background_tasks_stopped":true}))
    }

    #[test]
    fn native_binding_ticket_arguments_replay_and_revision_are_fenced() {
        let (_temp, mut c) = controller();
        assert!(
            c.handle(json!({"op":"bind","slot":2,"agent_id":"agent-1"}))
                .is_err()
        );
        assert!(
            c.handle(json!({"op":"bind","slot":1,"agent_id":"replacement"}))
                .is_err()
        );
        let args = json!({});
        let ticket = reserve(&mut c, 1, "call-1", "delm_status", args.clone());
        let forged = json!({"op":"consume","ticket":ticket,"tool":"delm_status","arguments":{"finding":"changed"}});
        assert!(c.handle(forged).is_err());
        assert!(
            c.handle(json!({"op":"consume","ticket":ticket,"tool":"delm_status","arguments":args}))
                .is_err()
        );
        assert!(c.handle(json!({"op":"reserve","agent_id":"agent-1","turn_id":"turn-1","call_id":"call-1","tool":"delm_status","arguments":{}})).is_err());
        let ticket = reserve(&mut c, 1, "call-2", "delm_status", json!({}));
        c.handle(json!({"op":"update","text":"Also support the changed requirement"}))
            .unwrap();
        assert!(
            c.handle(json!({"op":"consume","ticket":ticket,"tool":"delm_status","arguments":{}}))
                .is_err()
        );
        assert!(
            c.handle(json!({"op":"step","agent_id":"agent-1","turn_id":"turn-1","revision":1}))
                .is_err()
        );
        c.handle(json!({"op":"step","agent_id":"agent-1","turn_id":"turn-1","revision":2}))
            .unwrap();
        tool(&mut c, 1, "call-3", "delm_status", json!({}));
        let state = fs::read_to_string(c.workspace.run_dir.join("claude.json")).unwrap();
        assert!(
            !state.contains(&ticket),
            "one-use native capabilities must not be persisted"
        );
    }

    #[test]
    fn native_evidence_requires_an_observed_foreground_result_and_cannot_cross_workers() {
        let (_temp, mut c) = controller();
        let root = c.workspace.workers[0].clone();
        c.handle(json!({"op":"command_start","agent_id":"agent-1","turn_id":"turn-1","call_id":"bash-1","command":"check source","cwd":root})).unwrap();
        assert!(c.handle(json!({"op":"command_end","agent_id":"agent-2","call_id":"bash-1","result_ref":"7","is_error":false,"interrupted":false,"background_task_id":null,"timed_out":false})).is_err());
        c.handle(json!({"op":"command_end","agent_id":"agent-1","call_id":"bash-1","result_ref":"7","is_error":false,"interrupted":false,"background_task_id":"native-bg","timed_out":false})).unwrap();
        let declaration = json!({"idempotency_key":"done","expected_revision":1,"outcome":"complete","summary":"Ready","checks":["bash-1"]});
        let ticket = reserve(&mut c, 1, "done", "delm_complete", declaration.clone());
        assert!(c.handle(json!({"op":"consume","ticket":ticket,"tool":"delm_complete","arguments":declaration})).is_err());
        let native = serde_json::to_value(&c.workers[0].checks["bash-1"]).unwrap();
        assert!(native["completion"].get("exit_code").is_none());
    }

    #[test]
    fn candidate_delivers_only_after_native_shutdown_and_removes_private_workspaces() {
        let (_temp, mut c) = controller();
        complete(&mut c);
        assert_eq!(
            fs::read_to_string(c.workspace.original.join("source.txt")).unwrap(),
            "original\n"
        );
        assert!(c.handle(json!({"op":"settle","agents":[{"id":"agent-1","status":"completed"}],"background_tasks_stopped":true})).is_err());
        assert!(c.handle(json!({"op":"settle","agents":[{"id":"agent-1","status":"completed"},{"id":"agent-2","status":"running"}],"background_tasks_stopped":true})).is_err());
        let result = settle(&mut c).unwrap();
        assert_eq!(result["actions"][0]["status"], "complete");
        assert!(
            result["actions"][0]["delivery"]["cleanup_complete"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(
            fs::read_to_string(c.workspace.original.join("source.txt")).unwrap(),
            "finished\n"
        );
        assert!(c.workspace.workers.iter().all(|path| !path.exists()));
        assert!(c.finished());
    }

    #[test]
    fn a_changed_candidate_is_preserved_without_overwriting_the_original() {
        let (_temp, mut c) = controller();
        complete(&mut c);
        fs::write(c.workspace.workers[0].join("source.txt"), "late writer\n").unwrap();
        let final_result = settle(&mut c).unwrap();
        assert_eq!(c.status, "recovery_required");
        assert!(c.finished());
        assert!(c.workspace.workers.iter().all(|path| !path.exists()));
        assert!(
            Path::new(
                final_result["actions"][0]["recovery"]["recovery"]
                    .as_str()
                    .unwrap()
            )
            .exists()
        );
        assert_eq!(
            fs::read_to_string(c.workspace.original.join("source.txt")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn cancellation_preserves_partial_sources_and_cleans_only_after_native_stop() {
        let (_temp, mut c) = controller();
        fs::write(
            c.workspace.workers[1].join("partial.txt"),
            "useful progress\n",
        )
        .unwrap();
        c.handle(json!({"op":"cancel","reason":"User stopped"}))
            .unwrap();
        let result = settle(&mut c).unwrap();
        assert_eq!(result["actions"][0]["status"], "stopped");
        let recovery = PathBuf::from(
            result["actions"][0]["recovery"]["recovery"]
                .as_str()
                .unwrap(),
        );
        assert!(recovery.exists());
        assert!(c.workspace.workers.iter().all(|path| !path.exists()));
        assert!(!c.workspace.original.join("partial.txt").exists());
    }

    #[test]
    fn native_turn_completion_and_declaration_are_both_required_and_turns_cannot_reactivate() {
        let (_temp, mut c) = controller();
        let reply=c.handle(json!({"op":"turn_end","agent_id":"agent-1","turn_id":"turn-1","reason":"completed","answer":"All done"})).unwrap();
        assert_eq!(reply["actions"][0]["type"], "resume");
        assert!(c.candidate.is_none());
        assert!(
            c.handle(json!({"op":"step","agent_id":"agent-1","turn_id":"turn-1","revision":1}))
                .is_err()
        );
        assert!(c.handle(json!({"op":"turn_end","agent_id":"agent-1","turn_id":"turn-1","reason":"completed","answer":"All done"})).is_err());
        c.handle(json!({"op":"step","agent_id":"agent-1","turn_id":"turn-new","revision":1}))
            .unwrap();
    }
    #[test]
    fn orphan_recovery_authenticates_native_stop_then_preserves_work_and_reopens_admission() {
        let (temp, mut c) = controller();
        let token = "test-private-recovery-token";
        // Test an actual exited identity, never fabricate process existence.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("0.15")
            .spawn()
            .unwrap();
        let runtime = ProcessIdentity::capture(child.id()).unwrap();
        child.wait().unwrap();
        c.runtime = runtime;
        c.token_digest = Some(format!("{:x}", Sha256::digest(token)));
        c.persist().unwrap();
        let project = c.workspace.original.clone();
        let run_dir = c.workspace.run_dir.clone();
        let roots = c.workspace.workers.clone();
        fs::write(roots[1].join("partial.txt"), "preserve this progress").unwrap();
        assert!(state::check_native_admission(&project, temp.path(), None).is_err());
        drop(c);
        let mut request = json!({"token":token,"session_id":"session","agents":[{"id":"agent-1","status":"completed"},{"id":"agent-2","status":"killed"}],"background_tasks_stopped":true});
        request["token"] = json!("wrong");
        assert!(recover_at(&run_dir, &request).is_err());
        request["token"] = json!(token);
        request["agents"][1]["status"] = json!("running");
        assert!(recover_at(&run_dir, &request).is_err());
        assert!(roots.iter().all(|root| root.exists()));
        request["agents"][1]["status"] = json!("killed");
        let result = recover_at(&run_dir, &request).unwrap();
        assert_eq!(result["status"], "stopped");
        assert!(roots.iter().all(|root| !root.exists()));
        assert!(Path::new(result["recovery"]["recovery"].as_str().unwrap()).exists());
        assert!(!project.join("partial.txt").exists());
        state::check_native_admission(&project, temp.path(), None).unwrap();
        assert_eq!(
            recover_at(&run_dir, &request).unwrap()["already_finished"],
            true
        );
    }

    #[test]
    fn recovery_refuses_a_live_bridge_and_transport_identity_cannot_change() {
        let (_temp, mut c) = controller();
        let token = "live-bridge-secret";
        c.token_digest = Some(format!("{:x}", Sha256::digest(token)));
        c.persist().unwrap();
        assert!(
            c.handle(json!({"op":"transport","token_digest":"0".repeat(64)}))
                .is_err()
        );
        let request = json!({"token":token,"session_id":"session","agents":[{"id":"agent-1","status":"killed"},{"id":"agent-2","status":"killed"}],"background_tasks_stopped":true});
        assert!(
            recover_at(&c.workspace.run_dir, &request)
                .unwrap_err()
                .to_string()
                .contains("still running")
        );
        assert!(c.workspace.workers.iter().all(|root| root.exists()));
    }
    #[test]
    fn tokenless_recovery_requires_a_dead_host_and_registered_services_must_stop() {
        let (_temp, mut c) = controller();
        let mut ended = std::process::Command::new("/bin/sleep")
            .arg("0.1")
            .spawn()
            .unwrap();
        let dead = ProcessIdentity::capture(ended.id()).unwrap();
        ended.wait().unwrap();
        c.runtime = dead;
        c.persist().unwrap();
        assert!(
            recover_at(&c.workspace.run_dir, &json!({}))
                .unwrap_err()
                .to_string()
                .contains("saved native host")
        );
        c.native_host = dead;
        c.persist().unwrap();
        let run = c.workspace.run_dir.clone();
        let mut service = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let service_id = ProcessIdentity::capture(service.id()).unwrap();
        assert!(ensure_services_stopped(&[service_id]).is_err());
        assert!(service_id.is_running().unwrap());
        let mut saved = read_state(&run.join("claude.json")).unwrap();
        saved["services"] = json!([service_id]);
        atomic_json(&run.join("claude.json"), &saved).unwrap();
        drop(c);
        assert!(
            recover_at(&run, &json!({}))
                .unwrap_err()
                .to_string()
                .contains("Registered preview")
        );
        service.stdin.take();
        service.wait().unwrap();
        assert_eq!(recover_at(&run, &json!({})).unwrap()["status"], "stopped");
    }
    #[test]
    fn construction_failure_after_capture_cleans_copies_and_keeps_recovery_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("source.txt"), "original").unwrap();
        let project = project.canonicalize().unwrap();
        let lock = RunLock::acquire(&project).unwrap();
        let workspace = workspace::prepare(&project, &temp.path().join("run"), 1_000_000).unwrap();
        let roots = workspace.workers.clone();
        let run = workspace.run_dir.clone();
        fs::write(run.join("board"), "do not replace this obstruction").unwrap();
        let runtime = ProcessIdentity::capture(std::process::id()).unwrap();
        assert!(
            Controller::from_workspace(
                lock,
                workspace,
                "session".into(),
                "Task".into(),
                runtime,
                runtime
            )
            .is_err()
        );
        assert!(roots.iter().all(|root| !root.exists()));
        let saved = read_state(&run.join("claude.json")).unwrap();
        assert_eq!(saved["status"], "startup_failed");
        assert_eq!(saved["finished"], true);
        assert!(Path::new(saved["recovery"]["recovery"].as_str().unwrap()).exists());
        assert_eq!(
            fs::read_to_string(project.join("source.txt")).unwrap(),
            "original"
        );
        assert_eq!(
            fs::read_to_string(run.join("board")).unwrap(),
            "do not replace this obstruction"
        );
        state::check_native_admission(&project, temp.path(), None).unwrap();
        RunLock::acquire(&project).unwrap();
    }

    #[test]
    fn unpublished_handoff_rolls_back_and_published_worker_bindings_cannot_use_it() {
        let (_temp, mut c) = prepared_controller();
        assert!(c.workers.iter().all(|w| w.agent_id.is_none()));
        let result = c.abort_before_handoff("socket setup failed").unwrap();
        assert_eq!(result["status"], "startup_failed");
        assert!(c.finished());
        assert!(c.workspace.workers.iter().all(|root| !root.exists()));
        assert_eq!(
            read_state(&c.workspace.run_dir.join("claude.json")).unwrap()["finished"],
            true
        );
        let (_temp, mut bound) = controller();
        assert!(bound.abort_before_handoff("too late").is_err());
        assert!(bound.workspace.workers.iter().all(|root| root.exists()));
    }

    #[test]
    fn post_quiet_delivery_failure_keeps_the_transaction_journal_and_compacts_workspaces() {
        let (_temp, mut c) = controller();
        complete(&mut c);
        let delivery = c.workspace.run_dir.join("workspace/delivery");
        fs::create_dir(&delivery).unwrap();
        let journal = delivery.join("journal.json");
        let bytes = b"interrupted delivery metadata must stay intact";
        fs::write(&journal, bytes).unwrap();
        let result = settle(&mut c).unwrap();
        assert_eq!(result["actions"][0]["status"], "recovery_required");
        assert!(
            result["actions"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("interrupted delivery journal")
        );
        assert_eq!(fs::read(&journal).unwrap(), bytes);
        assert!(c.workspace.workers.iter().all(|root| !root.exists()));
        assert_eq!(
            fs::read_to_string(c.workspace.original.join("source.txt")).unwrap(),
            "original\n"
        );
        assert!(c.finished());
    }

    #[test]
    fn a_live_reference_before_quiet_preserves_copies_and_does_not_signal_the_process() {
        let (_temp, mut c) = controller();
        complete(&mut c);
        let mut child = std::process::Command::new("/bin/cat")
            .current_dir(&c.workspace.workers[0])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let process = ProcessIdentity::capture(child.id()).unwrap();
        assert!(settle(&mut c).is_err());
        assert!(!c.finished());
        assert!(c.workspace.workers.iter().all(|root| root.exists()));
        assert!(process.is_running().unwrap());
        child.stdin.take();
        child.wait().unwrap();
        assert_eq!(settle(&mut c).unwrap()["actions"][0]["status"], "complete");
    }

    #[test]
    fn action_ids_distinguish_events_and_are_stable_for_response_stream_duplicates() {
        let (_temp, mut c) = controller();
        let result = c
            .handle(json!({"op":"update","text":"New requirement"}))
            .unwrap();
        let duplicate = result.clone();
        assert_eq!(result["actions"][0]["id"], duplicate["actions"][0]["id"]);
        assert_ne!(result["actions"][0]["id"], result["actions"][1]["id"]);
        let next = c
            .handle(json!({"op":"update","text":"Another requirement"}))
            .unwrap();
        assert_ne!(result["actions"][0]["id"], next["actions"][0]["id"]);
        let mut nested = json!({"result":{"actions":[{"type":"resume"}]},"body":{"actions":[{"type":"candidate"}]}});
        stamp_actions(&mut nested, 42, &mut 0);
        assert!(
            nested["result"]["actions"][0]["id"]
                .as_str()
                .unwrap()
                .starts_with("42-")
        );
        assert_ne!(
            nested["result"]["actions"][0]["id"],
            nested["body"]["actions"][0]["id"]
        );
    }
    #[test]
    fn updates_choose_one_native_delivery_route_and_wait_for_worker_acknowledgment() {
        let (_temp, mut c) = controller();
        let reply = c
            .handle(json!({"op":"update","text":"Changed request"}))
            .unwrap();
        assert_eq!(reply["actions"].as_array().unwrap().len(), 2);
        assert!(
            reply["actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|action| action["type"] == "context")
        );
        for slot in 1..=2 {
            let ended = c.handle(json!({"op":"turn_end","agent_id":format!("agent-{slot}"),"turn_id":format!("turn-{slot}"),"reason":"completed","answer":"Old work ended"})).unwrap();
            assert!(ended["actions"].as_array().unwrap().is_empty());
        }
        assert!(
            !c.stop_requested,
            "Update delivery is pending even though the old turns ended"
        );
        let reply = c
            .handle(json!({"op":"update","text":"Latest request"}))
            .unwrap();
        assert!(
            reply["actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|action| action["type"] == "resume")
        );
        assert!(
            c.handle(json!({"op":"step","agent_id":"agent-1","turn_id":"new-turn","revision":2}))
                .is_err()
        );
        c.handle(json!({"op":"step","agent_id":"agent-1","turn_id":"new-turn","revision":3}))
            .unwrap();
        assert!(!c.workers[0].resume_pending);
        assert!(c.workers[1].resume_pending);
    }
    #[test]
    fn real_native_python_environment_captures_and_delivers_without_exported_read_grants() {
        let (_temp, mut c) = controller();
        let worker = c.workspace.workers[0].clone();
        let output = std::process::Command::new("python3")
            .args(["-m", "venv", "--without-pip"])
            .arg(worker.join(".venv"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(c.workers[0].result_policy.readonly_runtime_roots.is_empty());
        assert!(c.workers[0].result_policy.native_python_runtime);
        let captured =
            workspace::manifest_for_result(&worker, &c.workers[0].result_policy).unwrap();
        assert!(!captured.runtime_links.is_empty());
        complete(&mut c);
        let result = settle(&mut c).unwrap();
        assert_eq!(result["actions"][0]["delivery"]["delivered"], true);
        assert!(c.workspace.workers.iter().all(|root| !root.exists()));
        assert!(!c.workspace.original.join(".venv").exists());
        assert_eq!(
            fs::read_to_string(c.workspace.original.join("source.txt")).unwrap(),
            "finished\n"
        );
    }

    #[test]
    fn native_python_policy_keeps_config_source_and_original_peer_routes_strict() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (_temp, c) = controller();
        let external = tempfile::tempdir().unwrap();
        let runtime = external.path().canonicalize().unwrap();
        let interpreter = runtime.join("python3");
        fs::write(&interpreter, "fixture executable bytes").unwrap();
        fs::set_permissions(&interpreter, fs::Permissions::from_mode(0o755)).unwrap();
        let worker = &c.workspace.workers[0];
        let policy = &c.workers[0].result_policy;
        fs::create_dir_all(worker.join(".venv/bin")).unwrap();
        let link = worker.join(".venv/bin/python3");
        symlink(&interpreter, &link).unwrap();
        assert!(
            workspace::manifest_for_result(worker, policy).is_err(),
            "A directory named .venv is not enough"
        );
        let config = format!(
            "home = {}\ninclude-system-site-packages = false\nversion = 3.14.0\n",
            runtime.display()
        );
        fs::write(worker.join(".venv/pyvenv.cfg"), &config).unwrap();
        assert!(workspace::manifest_for_result(worker, policy).is_ok());
        assert!(workspace::manifest_for_result(worker, &ResultPolicy::default()).is_err());
        let legacy: ResultPolicy =
            serde_json::from_value(json!({"readonly_runtime_roots":[],"denied_roots":[]})).unwrap();
        assert!(!legacy.native_python_runtime);
        for target in [&interpreter, &worker.join(".venv/bin/python3")] {
            symlink(target, worker.join("source-link")).unwrap();
            assert!(workspace::manifest_for_result(worker, policy).is_err());
            fs::remove_file(worker.join("source-link")).unwrap();
        }
        for denied in [
            &c.workspace.original,
            &c.workspace.workers[1],
            &c.workspace.run_dir,
        ] {
            let route = denied.join("python3");
            symlink(&interpreter, &route).unwrap();
            fs::remove_file(&link).unwrap();
            symlink(&route, &link).unwrap();
            assert!(
                workspace::manifest_for_result(worker, policy).is_err(),
                "Interpreter route through {} must remain denied",
                denied.display()
            );
            fs::remove_file(route).unwrap();
        }
        fs::remove_file(&link).unwrap();
        symlink(&interpreter, &link).unwrap();
        fs::remove_file(worker.join(".venv/pyvenv.cfg")).unwrap();
        fs::write(worker.join("other-config"), config).unwrap();
        symlink("../other-config", worker.join(".venv/pyvenv.cfg")).unwrap();
        assert!(workspace::manifest_for_result(worker, policy).is_err());
    }
    #[test]
    fn late_native_binding_receives_cumulative_updates_once_before_revision_acknowledgment() {
        let (_temp, mut c) = prepared_controller();
        let initial = c
            .handle(json!({"op":"bind","slot":1,"agent_id":"first"}))
            .unwrap();
        assert!(initial["actions"].as_array().unwrap().is_empty());
        c.handle(json!({"op":"update","text":"Include the first update"}))
            .unwrap();
        c.handle(json!({"op":"update","text":"Include the second update"}))
            .unwrap();
        let binding = c
            .handle(json!({"op":"bind","slot":2,"agent_id":"late"}))
            .unwrap();
        let actions = binding["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["type"], "context");
        assert_eq!(actions[0]["agent_id"], "late");
        assert_eq!(actions[0]["revision"], 3);
        assert_eq!(actions[0]["message"], c.task);
        assert!(
            actions[0]["message"]
                .as_str()
                .unwrap()
                .contains("first update")
        );
        assert!(
            actions[0]["message"]
                .as_str()
                .unwrap()
                .contains("second update")
        );
        assert!(actions[0]["id"].is_string());
        assert!(c.workers[1].resume_pending);
        assert_eq!(
            c.workers[1].revision, 0,
            "Binding is not proof of native context delivery"
        );
        let replay = c
            .handle(json!({"op":"bind","slot":2,"agent_id":"late"}))
            .unwrap();
        assert!(replay["actions"].as_array().unwrap().is_empty());
        assert!(
            c.handle(json!({"op":"step","agent_id":"late","turn_id":"stale","revision":1}))
                .is_err()
        );
        assert!(c.workers[1].resume_pending);
        c.handle(json!({"op":"step","agent_id":"late","turn_id":"fresh","revision":3}))
            .unwrap();
        assert!(!c.workers[1].resume_pending);
        assert_eq!(c.workers[1].revision, 3);
    }
}
