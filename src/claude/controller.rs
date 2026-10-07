//! Host-specific lifecycle over the shared board, evidence, and delivery engines.
//! Only the authenticated native module may send control operations. MCP calls
//! consume one-use tickets reserved by that module before native permission UI.
use crate::{
    board::{Board, WaitCursor},
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
    wait_cursor: Option<WaitCursor>,
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

#[derive(Clone, Serialize)]
struct Finalization {
    generation: u64,
    revision: u64,
    intent: &'static str,
    reason: &'static str,
    started: bool,
    attempt: u64,
    shutdown_ack: &'static str,
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
    worker_scopes: [Option<Vec<FilesystemScope>>; 2],
    board: Board,
    services: Services,
    tickets: HashMap<String, Ticket>,
    candidate: Option<(usize, Completion)>,
    finalization: Option<Finalization>,
    final_result: Option<Value>,
    host_version: Option<String>,
    package_root: Option<PathBuf>,
    retained_artifacts: Vec<String>,
    journal: Journal,
    stop_requested: bool,
    finished: bool,
    status: String,
    token_digest: Option<String>,
    admission_recorded: bool,
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
        let mut preparation_journal = Journal::open(&run_dir)?;
        preparation_journal.observe("phase", &json!({"phase":"preparation","boundary":"start"}))?;
        let prepared = workspace::prepare(&project, &run_dir, crate::config::MAX_REPO_SIZE_BYTES);
        preparation_journal.observe(
            "phase",
            &json!({"phase":"preparation","boundary":"end","success":prepared.is_ok()}),
        )?;
        let workspace = match prepared {
            Ok(workspace) => workspace,
            Err(error) => {
                // The preparation guard normally removes its captures. If any
                // directory remains, keep admission closed instead of inferring
                // ownership from its name or deleting uncertain data.
                let captures = run_dir.join("workspace");
                let clean = !captures.try_exists()?
                    || fs::read_dir(&captures)?.all(|entry| {
                        entry.is_ok_and(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
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
            worker_scopes: [None, None],
            board,
            services: Services::default(),
            tickets: HashMap::new(),
            candidate: None,
            finalization: None,
            final_result: None,
            host_version: None,
            package_root: None,
            retained_artifacts: Vec::new(),
            journal,
            stop_requested: false,
            finished: false,
            status: "prepared".into(),
            token_digest: None,
            admission_recorded: false,
        };
        result.journal.record("claude_prepared", &result.ready())?;
        result.journal.observe(
            "phase",
            &json!({"phase":"worker_admission","boundary":"start"}),
        )?;
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
            "task":self.task,"status":self.status,"finalization":self.finalization})
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
        let coordination_worker = if request["op"] == "consume" {
            request["ticket"]
                .as_str()
                .and_then(|ticket| self.tickets.get(ticket))
                .map(|ticket| ticket.worker + 1)
        } else {
            None
        };
        let mut result = self.dispatch(&request);
        if let Ok(body) = &mut result {
            let mut index = 0;
            stamp_actions(body, self.sequence, &mut index);
            if let Some(worker) = coordination_worker {
                self.journal.observe(
                    "coordination_response",
                    &json!({"worker":worker,"bytes":serde_json::to_vec(body)?.len()}),
                )?;
            }
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
                self.host_version = request["host_version"]
                    .as_str()
                    .filter(|value| !value.is_empty() && value.len() <= 128)
                    .map(str::to_owned);
                self.package_root = request["package_root"]
                    .as_str()
                    .filter(|value| value.len() <= 16384 && Path::new(value).is_absolute())
                    .map(PathBuf::from);
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
                if !self.stop_requested && self.candidate.is_none() {
                    self.journal
                        .observe("phase", &json!({"phase":"shutdown","boundary":"start"}))?;
                }
                self.stop_requested = true;
                self.candidate = None;
                self.tickets.clear();
                self.status = "stopping".into();
                if self
                    .finalization
                    .as_ref()
                    .is_none_or(|value| value.intent != "cancel")
                {
                    self.prepare_finalization(
                        "cancel",
                        stop_reason(text(request, "reason", 4096)?),
                    );
                }
                self.journal.record(
                    "claude_cancelled",
                    &json!({"reason":text(request,"reason",4096)?}),
                )?;
                Ok(
                    json!({"actions":[{"type":"stop","reason":self.finalization.as_ref().unwrap().reason,"agents":self.bound_agents(),"finalization":self.finalization}]}),
                )
            }
            "interrupt" => {
                if self.candidate.is_none() {
                    return self
                        .dispatch(&json!({"op":"cancel","reason":text(request,"reason",4096)?}));
                }
                // Bridge loss is not a user cancellation. Keep the selected
                // result for authenticated recovery and stopped-writer checks.
                self.tickets.clear();
                self.status = "recovery_required".into();
                self.journal.record(
                    "claude_interrupted",
                    &json!({"reason":text(request,"reason",4096)?}),
                )?;
                Ok(json!({"finalization":self.finalization}))
            }
            "begin_settle" => {
                self.require_finalization(request)?;
                let finalization = self.finalization.as_mut().unwrap();
                finalization.started = true;
                finalization.attempt = finalization.attempt.saturating_add(1);
                finalization.shutdown_ack = "pending";
                self.status = if finalization.intent == "deliver" {
                    "awaiting_shutdown"
                } else {
                    "stopping"
                }
                .into();
                Ok(json!({"finalization":self.finalization}))
            }
            "settlement_failed" => {
                self.require_finalization(request)?;
                self.finalization.as_mut().unwrap().shutdown_ack = "unconfirmed";
                self.status = "recovery_required".into();
                Ok(json!({"finalization":self.finalization}))
            }
            "settle" => self.settle(request),
            "status" => Ok(
                json!({"run":self.ready(),"board":self.board.view()?,"agents":self.workers,
                "candidate":self.candidate.as_ref().map(|(i,_)|self.workers[*i].agent_id.clone()),"finished":self.finished,"final":self.final_result}),
            ),
            op => bail!("Unknown native control operation: {op}"),
        }
    }

    fn prepare_finalization(&mut self, intent: &'static str, reason: &'static str) {
        self.finalization = Some(Finalization {
            generation: self.sequence,
            revision: self.revision,
            intent,
            reason,
            started: false,
            attempt: 0,
            shutdown_ack: "pending",
        });
    }

    fn require_finalization(&self, request: &Value) -> Result<()> {
        let current = self
            .finalization
            .as_ref()
            .context("Finalization is obsolete; refresh the run state")?;
        ensure!(
            request["generation"].as_u64() == Some(current.generation)
                && request["revision"].as_u64() == Some(current.revision),
            "Finalization is obsolete; refresh the run state"
        );
        ensure!(
            self.stop_requested || self.candidate.is_some(),
            "Run has no completed result or stop request"
        );
        Ok(())
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
            if self.workers[i].turn_id.is_none() {
                self.journal.observe(
                    "worker_turn_started",
                    &json!({"worker":i+1,"turn_id":turn,"revision":self.revision}),
                )?;
            }
            self.workers[i].outcome = None;
            self.workers[i].waiting = false;
            self.workers[i].wait_cursor = None;
            self.workers[i].blocked = false;
        }
        self.workers[i].resume_pending = false;
        self.workers[i].turn_id = Some(turn.into());
        self.workers[i].revision = self.revision;
        if !self.admission_recorded && self.workers.iter().all(|worker| worker.revision > 0) {
            self.admission_recorded = true;
            self.journal.observe(
                "phase",
                &json!({"phase":"worker_admission","boundary":"end","success":true}),
            )?;
        }
        Ok(json!({"revision":self.revision,"task":self.task}))
    }

    fn configure_scopes(&mut self, r: &Value) -> Result<Value> {
        let i = self.worker(r)?;
        let scopes: Vec<FilesystemScope> = serde_json::from_value(r["scopes"].clone())?;
        self.worker_scopes[i] = Some(self.board.set_worker_scopes(i + 1, scopes)?);
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
            if self.workers[i].outcome.as_ref() != Some(&args) {
                self.workers[i].wait_cursor = self.board.wait_cursor(&args)?;
            }
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
        if tool == "delm_complete" {
            actions.push(json!({"type":"completion_intent","agent_id":self.workers[i].agent_id,
                "turn_id":self.workers[i].turn_id,"revision":self.revision,"pending":args["outcome"] == "complete"}));
        }
        if matches!(
            tool,
            "delm_publish"
                | "delm_task_finish"
                | "delm_task_release"
                | "delm_task_split"
                | "delm_task_create"
                | "delm_complete"
        ) {
            self.wake_waiting(&mut actions,"Shared work changed. Read the board, take ready work, and reuse the new contribution.")?;
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
        self.journal.observe("worker_turn_finished",&json!({"worker":i+1,"turn_id":turn,"status":reason,"revision":self.workers[i].revision,"waiting":reason=="completed" && self.workers[i].revision==self.revision && self.workers[i].outcome.as_ref().is_some_and(|value|value["outcome"]=="waiting")}))?;
        let completing = self.workers[i]
            .outcome
            .as_ref()
            .is_some_and(|value| value["outcome"] == "complete");
        let mut actions = Vec::new();
        if reason != "completed" {
            self.workers[i].blocked = true;
            self.board.release_worker_claims(i + 1, reason)?;
            self.services.retire_worker(i + 1)?;
            self.wake_waiting(&mut actions,"Your peer stopped. Read the released tasks and continue useful work from its published contributions.")?;
        } else if !self.stop_requested
            && self.workers[i].revision == self.revision
            && self.candidate.is_none()
        {
            let declaration = self.workers[i].outcome.clone();
            match declaration.as_ref().and_then(|value|value["outcome"].as_str()) {
                Some("complete")=>{
                    ensure!(!answer.trim().is_empty(),"Native completed turn has no final answer");
                    let declaration=declaration.as_ref().unwrap();
                    let completion=Completion::capture_with_evidence(&self.board,i+1,declaration,&self.workers[i].checks,self.revision,&self.workers[i].result_policy,&self.workspace.baseline_manifest)?;
                    atomic_json(&self.workspace.run_dir.join("completion.json"),&completion)?;
                    if let Some(accepted) = &completion.accepted {
                        self.retained_artifacts.extend(accepted.selection.artifacts.iter().cloned());
                        self.retained_artifacts.sort();
                        self.retained_artifacts.dedup();
                    }
                    self.candidate=Some((i,completion));
                    self.prepare_finalization("deliver", "completed_candidate");
                    self.journal.observe("phase", &json!({"phase":"shutdown","boundary":"start"}))?;
                    self.status="awaiting_shutdown".into();
                    self.tickets.clear();
                    actions.push(json!({"type":"candidate","agent_id":self.workers[i].agent_id,"revision":self.revision,"finalization":self.finalization}));
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
        self.wake_waiting(
            &mut actions,
            "Shared work changed. Read the board, take ready work, and reuse the new contribution.",
        )?;
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
                self.journal
                    .observe("phase", &json!({"phase":"shutdown","boundary":"start"}))?;
                self.status = "stopping".into();
                self.prepare_finalization("cancel", "no_active_workers");
                actions.push(json!({"type":"stop","reason":"No active worker can continue","agents":self.bound_agents(),"finalization":self.finalization}));
            }
        }
        if completing {
            actions.push(
                json!({"type":"completion_intent","agent_id":self.workers[i].agent_id,
                "turn_id":turn,"revision":self.workers[i].revision,"pending":false}),
            );
        }
        Ok(json!({"actions":actions}))
    }

    fn resume(&mut self, i: usize, message: &str) -> Value {
        self.workers[i].resume_pending = true;
        json!({"type":"resume","agent_id":self.workers[i].agent_id,"revision":self.revision,"message":message})
    }
    fn wake_waiting(&mut self, actions: &mut Vec<Value>, message: &str) -> Result<()> {
        if self.stop_requested
            || self.candidate.is_some()
            || self.workers.iter().any(|worker| {
                worker.turn_id.is_some()
                    && worker.revision == self.revision
                    && worker
                        .outcome
                        .as_ref()
                        .is_some_and(|value| value["outcome"] == "complete")
            })
        {
            return Ok(());
        }
        for i in 0..2 {
            if self.workers[i].waiting
                && !self.workers[i].blocked
                && !self.workers[i].resume_pending
                && self.workers[i].turn_id.is_none()
                && self.workers[i].revision == self.revision
                && let Some(cursor) = &self.workers[i].wait_cursor
                && self.board.wait_ready(cursor)?
            {
                self.workers[i].waiting = false;
                actions.push(self.resume(i, message));
            }
        }
        Ok(())
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
            "DeLM is stopping. This update was not delivered; wait for recovery before retrying it"
        );
        ensure!(
            !self
                .finalization
                .as_ref()
                .is_some_and(|value| value.started),
            "DeLM is finishing delivery. This update was not delivered; retry it after finishing"
        );
        let update = text(r, "text", 128 * 1024)?;
        self.revision = self
            .revision
            .checked_add(1)
            .context("Task revision overflow")?;
        self.task.push_str(&format!("\n\nUser update:\n{update}"));
        self.board.set_revision(self.revision)?;
        self.candidate = None;
        self.finalization = None;
        self.tickets.clear();
        for worker in &mut self.workers {
            worker.outcome = None;
            worker.waiting = false;
            worker.wait_cursor = None;
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
        self.require_finalization(r)?;
        ensure!(
            self.finalization.as_ref().unwrap().started,
            "Native finalization has not begun"
        );
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
            self.finalization.as_mut().unwrap().shutdown_ack = "confirmed";
            self.journal.observe(
                "phase",
                &json!({"phase":"shutdown","boundary":"end","success":true}),
            )?;
            for (i, worker) in self.workers.iter().enumerate() {
                if let Some(turn) = &worker.turn_id {
                    self.journal.observe("worker_turn_finished", &json!({"worker":i+1,"turn_id":turn,"status":"stopped","revision":worker.revision}))?;
                }
            }
            if let Some((i, candidate)) = &self.candidate {
                ensure!(
                    self.workers.iter().all(|worker| worker.agent_id.is_some()),
                    "Both native peers must be bound before result delivery"
                );
                ensure!(
                    !self.stop_requested && candidate.revision == self.revision,
                    "Candidate is obsolete or cancelled"
                );
                self.journal.observe(
                    "phase",
                    &json!({"phase":"delivery_and_cleanup","boundary":"start"}),
                )?;
                let delivery = candidate.deliver_with_validation(&self.workspace, *i, || {
                    candidate.verify_shared_inputs(&self.board, *i + 1)
                });
                self.journal.observe("phase", &json!({"phase":"delivery_and_cleanup","boundary":"end","success":delivery.is_ok()}))?;
                let delivery = delivery?;
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
                self.journal.observe(
                    "phase",
                    &json!({"phase":"recovery_and_cleanup","boundary":"start"}),
                )?;
                let recovery = workspace::preserve_partial_and_cleanup_with_artifacts(
                    &self.workspace,
                    &self.retained_artifacts,
                );
                self.journal.observe("phase", &json!({"phase":"recovery_and_cleanup","boundary":"end","success":recovery.is_ok()}))?;
                let recovery = recovery?;
                self.status = "stopped".into();
                Ok(json!({"type":"final","status":self.status,"recovery":recovery}))
            }
        })();
        match result {
            Ok(final_event) => {
                self.finished = true;
                self.final_result = Some(final_event.clone());
                self.tickets.clear();
                self.journal.record("claude_final", &final_event)?;
                Ok(json!({"actions":[final_event]}))
            }
            Err(error) => {
                if !quiet {
                    self.finalization.as_mut().unwrap().shutdown_ack = "unconfirmed";
                    self.journal.observe(
                        "phase",
                        &json!({"phase":"shutdown","boundary":"end","success":false}),
                    )?;
                }
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
                    match workspace::preserve_partial_and_cleanup_with_artifacts(&self.workspace, &self.retained_artifacts) {
                        Ok(recovery) => {
                            self.finished = true;
                            self.tickets.clear();
                            let final_event = json!({"type":"final","status":"recovery_required","reason":reason,
                                "recovery":recovery,"delivery_journal":self.workspace.run_dir.join("workspace/delivery/journal.json")});
                            self.final_result = Some(final_event.clone());
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
            &json!({"version":1,"host":"claude","runtime_version":env!("CARGO_PKG_VERSION"),"host_version":self.host_version,"package_root":self.package_root,"session_id":self.session_id,
            "workspace":self.workspace,"project":self.workspace.original,"token_digest":self.token_digest,"task":self.task,"revision":self.revision,"sequence":self.sequence,"native_host":self.native_host,
            "runtime":self.runtime,"workers":self.workers,"worker_scopes":self.worker_scopes,"services":self.services.process_identities(),"candidate":self.candidate,"status":self.status,
            "stop_requested":self.stop_requested,"finished":self.finished,"finalization":self.finalization,"final_result":self.final_result,"retained_artifacts":self.retained_artifacts}),
        )
    }
}

fn stop_reason(reason: &str) -> &'static str {
    match reason {
        "User requested /delm-stop" | "User stopped" => "user_cancelled",
        "DeLM's execution allowance expired" => "deadline",
        "The native Claude host exited" => "host_exited",
        "Claude unloaded its DeLM bridge; native worker shutdown is unconfirmed" => {
            "bridge_unloaded"
        }
        "Native peer launch failed"
        | "Claude did not complete the required two-peer native launch." => "launch_failed",
        _ if reason.starts_with("Conversation") => "conversation_ended",
        _ if reason.starts_with("Native launch output") => "transport_failed",
        _ => "cancelled",
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

/// Recovery requires native ownership proof and quiet private workspaces.
/// An explicitly retained delivery intent may deliver only the same revision
/// and revalidated candidate; intentional cancellation only preserves changes.
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
    let mut artifacts: Vec<String> = serde_json::from_value(
        saved
            .get("retained_artifacts")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )?;
    if !saved["candidate"].is_null() {
        let (_, candidate): (usize, Completion) =
            serde_json::from_value(saved["candidate"].clone())?;
        if let Some(accepted) = candidate.accepted {
            artifacts.extend(accepted.selection.artifacts);
            artifacts.sort();
            artifacts.dedup();
        }
    }
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
        if saved["final_result"].is_object() {
            let mut result = saved["final_result"].clone();
            result["already_finished"] = json!(true);
            return Ok(result);
        }
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
    let retained_delivery = saved["stop_requested"] == false
        && saved["finalization"]["intent"] == "deliver"
        && !saved["candidate"].is_null();
    if request["intent"] == "deliver" || (request["intent"] == "auto" && retained_delivery) {
        ensure!(
            request["token"]
                .as_str()
                .is_some_and(|token| !token.is_empty()),
            "Delivery recovery requires the owning native conversation"
        );
        ensure!(
            saved["stop_requested"] == false && saved["finalization"]["intent"] == "deliver",
            "This run has no retained delivery intent; preserve its unfinished work"
        );
        ensure!(
            saved["workers"]
                .as_array()
                .is_some_and(|workers| workers.len() == 2
                    && workers
                        .iter()
                        .all(|worker| worker["agent_id"].as_str().is_some())),
            "Both native peers must be bound before recovery delivery"
        );
        let (worker, candidate): (usize, Completion) =
            serde_json::from_value(saved["candidate"].clone())
                .context("The selected result is unavailable; preserve this run for recovery")?;
        ensure!(
            worker < 2
                && saved["revision"].as_u64() == Some(candidate.revision)
                && saved["finalization"]["revision"].as_u64() == Some(candidate.revision),
            "The selected result belongs to an obsolete request revision"
        );
        // Delivery reconciles its durable completed transaction first. It
        // revalidates the candidate when applying for the first time; a retry
        // after cleanup must not require removed worker files or replay edits.
        let delivered = candidate.deliver_with_validation(&prepared, worker, || {
            // Older runs without shared receipts need no saved board scopes.
            // Never infer broader permissions for a receipt from an old run.
            let declared = candidate.declaration.get("shared_checks");
            if candidate.shared_checks.is_empty()
                && declared.is_none_or(|value| value.as_array().is_some_and(Vec::is_empty))
            {
                return Ok(());
            }
            let scopes: Vec<FilesystemScope> = serde_json::from_value(
                saved["worker_scopes"][worker].clone(),
            ).context("Saved run lacks native check-input permissions; its result must be preserved for recovery instead of automatic delivery")?;
            let mut board = Board::open(
                &prepared.run_dir,
                &prepared.baseline,
                prepared.workers.clone(),
            )?;
            ensure!(
                board.set_worker_scopes(worker + 1, scopes.clone())? == scopes,
                "Saved native check-input permissions resolve differently; preserve the result for recovery"
            );
            candidate.verify_shared_inputs(&board, worker + 1)
        });
        let result = match delivered {
            Ok(delivery) => {
                json!({"type":"final","status":if !delivery.delivered {"delivery_conflict"} else if delivery.verification_required {"delivered"} else {"complete"},
                "delivery":delivery,"summary":candidate.declaration["summary"],"checks":candidate.checks,"shared_checks":candidate.shared_checks})
            }
            Err(error) => {
                let recovery =
                    workspace::preserve_partial_and_cleanup_with_artifacts(&prepared, &artifacts)?;
                json!({"type":"final","status":"recovery_required","reason":format!("{error:#}"),"recovery":recovery})
            }
        };
        saved["status"] = result["status"].clone();
        saved["finished"] = json!(true);
        saved["final_result"] = result.clone();
        saved["finalization"]["shutdown_ack"] = json!("confirmed");
        atomic_json(&path, &saved)?;
        return Ok(result);
    }
    let recovery = workspace::preserve_partial_and_cleanup_with_artifacts(&prepared, &artifacts)?;
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
        let finalization = c
            .finalization
            .clone()
            .context("Missing test finalization")?;
        c.handle(json!({"op":"begin_settle","generation":finalization.generation,"revision":finalization.revision}))?;
        c.handle(json!({"op":"settle","generation":finalization.generation,"revision":finalization.revision,
            "agents":[{"id":"agent-1","status":"completed"},{"id":"agent-2","status":"killed"}],"background_tasks_stopped":true}))
    }

    fn complete_with_dependency_check(c: &mut Controller) {
        let root = c.workspace.workers[0].clone();
        fs::write(root.join("source.txt"), "finished\n").unwrap();
        fs::create_dir_all(root.join("node_modules/three/build")).unwrap();
        fs::write(
            root.join("node_modules/three/build/three.core.js"),
            "export const checkedDependency = true;\n",
        )
        .unwrap();
        fs::create_dir(root.join(".cache")).unwrap();
        fs::write(root.join(".gitignore"), ".cache/\nnode_modules/\n").unwrap();
        fs::write(root.join(".cache/requested.txt"), "requested artifact\n").unwrap();
        let snapshot = tool(
            c,
            1,
            "begin-dependency-check",
            "delm_check_begin",
            json!({"idempotency_key":"begin-dependency-check", "summary":"Check source with its installed dependency",
                "paths":["source.txt", ".cache/requested.txt", "node_modules/three/build/three.core.js", "node_modules/three/build/absent.js"]}),
        );
        c.handle(
            json!({"op":"command_start","agent_id":"agent-1","turn_id":"turn-1",
            "call_id":"dependency-check","command":"node check.mjs","cwd":root}),
        )
        .unwrap();
        c.handle(
            json!({"op":"command_end","agent_id":"agent-1","call_id":"dependency-check",
            "result_ref":"dependency-check-result","is_error":false,"interrupted":false,
            "background_task_id":null,"timed_out":false}),
        )
        .unwrap();
        let receipt = tool(
            c,
            1,
            "finish-dependency-check",
            "delm_check_finish",
            json!({"idempotency_key":"finish-dependency-check", "snapshot_id":snapshot["result"]["snapshot_id"],
                "command_id":"dependency-check"}),
        );
        assert_eq!(receipt["result"]["reusable"], true);
        tool(
            c,
            1,
            "complete-with-dependency",
            "delm_complete",
            json!({"idempotency_key":"complete-with-dependency", "expected_revision":1,"outcome":"complete",
                "summary":"Implemented with a checked dependency", "checks":["dependency-check"],
                "shared_checks":[receipt["result"]["receipt_id"]], "artifacts":[".cache/requested.txt"]}),
        );
        let result = c
            .handle(
                json!({"op":"turn_end","agent_id":"agent-1","turn_id":"turn-1",
            "reason":"completed","answer":"Implemented and checked."}),
            )
            .unwrap();
        assert_eq!(result["actions"][0]["type"], "candidate");
    }

    #[test]
    fn dependency_check_inputs_are_validated_without_delivering_installed_dependencies() {
        let (_temp, mut c) = controller();
        complete_with_dependency_check(&mut c);
        // Only named inputs back this receipt. Unrelated installed files are
        // neither delivery outputs nor evidence for the declared check.
        fs::write(
            c.workspace.workers[0].join("node_modules/three/unrelated.js"),
            "unrelated late write\n",
        )
        .unwrap();
        assert_dependency_check_delivered(&settle(&mut c).unwrap()["actions"][0]);
        assert_eq!(
            fs::read_to_string(c.workspace.original.join("source.txt")).unwrap(),
            "finished\n"
        );
        assert_eq!(
            fs::read_to_string(c.workspace.original.join(".cache/requested.txt")).unwrap(),
            "requested artifact\n"
        );
        assert!(!c.workspace.original.join("node_modules").exists());
        assert!(c.workspace.workers.iter().all(|root| !root.exists()));
    }

    fn assert_dependency_check_delivered(result: &Value) {
        // Existing delivery policy still requests verification when installed
        // environment files were intentionally omitted from the result.
        assert_eq!(result["status"], "delivered", "{result}");
        assert_eq!(result["delivery"]["delivered"], true);
        assert_eq!(result["delivery"]["verification_required"], true);
        assert_eq!(result["delivery"]["cleanup_complete"], true);
    }

    #[test]
    fn dependency_check_changes_after_selection_preserve_the_original_and_recoverable_outputs() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        for change in ["contents", "deleted", "mode", "absent_created", "symlink"] {
            let (temp, mut c) = controller();
            complete_with_dependency_check(&mut c);
            let root = c.workspace.workers[0].clone();
            let dependency = root.join("node_modules/three/build/three.core.js");
            match change {
                "contents" => fs::write(&dependency, "changed after check\n").unwrap(),
                "deleted" => fs::remove_file(&dependency).unwrap(),
                "mode" => {
                    let mode = fs::metadata(&dependency).unwrap().permissions().mode();
                    fs::set_permissions(&dependency, fs::Permissions::from_mode(mode ^ 0o100))
                        .unwrap();
                }
                "absent_created" => {
                    fs::write(
                        root.join("node_modules/three/build/absent.js"),
                        "new input\n",
                    )
                    .unwrap();
                }
                "symlink" => {
                    fs::remove_file(&dependency).unwrap();
                    symlink(root.join("source.txt"), &dependency).unwrap();
                }
                _ => unreachable!(),
            }
            let result = settle(&mut c).unwrap();
            assert_eq!(
                result["actions"][0]["status"], "recovery_required",
                "{change}: {result}"
            );
            assert!(
                result["actions"][0]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("shared check"),
                "{change}: {result}"
            );
            assert_eq!(
                fs::read_to_string(c.workspace.original.join("source.txt")).unwrap(),
                "original\n",
                "{change}"
            );
            assert!(!c.workspace.original.join(".cache/requested.txt").exists());
            assert!(!c.workspace.original.join("node_modules").exists());
            assert!(c.workspace.workers.iter().all(|root| !root.exists()));
            let recovery = Path::new(
                result["actions"][0]["recovery"]["recovery"]
                    .as_str()
                    .unwrap(),
            );
            let exported =
                workspace::export_recovery(recovery, &temp.path().join("exported"), 1).unwrap();
            assert_eq!(
                fs::read_to_string(exported.files.join("source.txt")).unwrap(),
                "finished\n"
            );
            assert_eq!(
                fs::read_to_string(exported.files.join(".cache/requested.txt")).unwrap(),
                "requested artifact\n"
            );
        }
    }

    #[test]
    fn dependency_check_candidate_keeps_retryable_shutdown_and_explicit_cancellation() {
        for cancel in [false, true] {
            let (_temp, mut c) = controller();
            complete_with_dependency_check(&mut c);
            let fence = c.finalization.clone().unwrap();
            c.handle(json!({"op":"begin_settle","generation":fence.generation,"revision":fence.revision}))
                .unwrap();
            c.handle(json!({"op":"settlement_failed","generation":fence.generation,"revision":fence.revision}))
                .unwrap();
            assert!(c.candidate.is_some());
            assert!(c.workspace.workers.iter().all(|root| root.exists()));
            if cancel {
                c.handle(json!({"op":"cancel","reason":"User requested /delm-stop"}))
                    .unwrap();
                assert!(c.candidate.is_none());
            }
            let result = settle(&mut c).unwrap();
            if cancel {
                assert_eq!(result["actions"][0]["status"], "stopped");
            } else {
                assert_dependency_check_delivered(&result["actions"][0]);
            }
            assert_eq!(
                fs::read_to_string(c.workspace.original.join("source.txt")).unwrap(),
                if cancel { "original\n" } else { "finished\n" }
            );
            assert!(!c.workspace.original.join("node_modules").exists());
            assert!(c.workspace.workers.iter().all(|root| !root.exists()));
        }
    }

    fn make_dependency_check_recoverable(c: &mut Controller) -> Value {
        // Hold a child alive with its input pipe, then close it after capturing
        // identity. This models an exited bridge without a timed sleep.
        let mut process = std::process::Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        c.runtime = ProcessIdentity::capture(process.id()).unwrap();
        drop(process.stdin.take());
        assert!(process.wait().unwrap().success());
        let token = "dependency-recovery-token";
        c.token_digest = Some(format!("{:x}", Sha256::digest(token)));
        c.persist().unwrap();
        json!({"intent":"auto","token":token,"session_id":"session",
            "agents":[{"id":"agent-1","status":"completed"},{"id":"agent-2","status":"killed"}],
            "background_tasks_stopped":true})
    }

    #[test]
    fn dependency_check_recovery_revalidates_inputs_and_saved_access_before_first_delivery() {
        for change in [
            "unchanged",
            "unrelated",
            "contents",
            "denied",
            "deny_alias_retargeted",
            "saved_policy_path_replaced",
            "receipt_scope_missing",
            "receipt_hash",
            "receipt_id",
            "malformed_declaration",
        ] {
            let (temp, mut c) = controller();
            complete_with_dependency_check(&mut c);
            let root = c.workspace.workers[0].clone();
            match change {
                "unchanged"
                | "receipt_scope_missing"
                | "receipt_hash"
                | "receipt_id"
                | "malformed_declaration" => {}
                "unrelated" => {
                    fs::write(root.join("node_modules/unrelated.js"), "unrelated\n").unwrap()
                }
                "contents" => fs::write(
                    root.join("node_modules/three/build/three.core.js"),
                    "changed\n",
                )
                .unwrap(),
                "denied" => {
                    c.handle(json!({"op":"configure_scopes","agent_id":"agent-1", "scopes":[
                        FilesystemScope {path:root.clone(), access:FilesystemAccess::Write},
                        FilesystemScope {path:root.join("node_modules/three"), access:FilesystemAccess::Deny}
                    ]})).unwrap();
                }
                "deny_alias_retargeted" => {
                    let alias = temp.path().join("permission-alias");
                    std::os::unix::fs::symlink(root.join("node_modules/three"), &alias).unwrap();
                    c.handle(
                        json!({"op":"configure_scopes","agent_id":"agent-1", "scopes":[
                            FilesystemScope {path:root.clone(), access:FilesystemAccess::Write},
                            FilesystemScope {path:alias.clone(), access:FilesystemAccess::Deny}
                        ]}),
                    )
                    .unwrap();
                    let unrelated = temp.path().join("unrelated-permission-target");
                    fs::create_dir(&unrelated).unwrap();
                    fs::remove_file(&alias).unwrap();
                    std::os::unix::fs::symlink(unrelated, &alias).unwrap();
                }
                "saved_policy_path_replaced" => {
                    let denied = temp.path().join("saved-denied-directory");
                    let replacement = temp.path().join("replacement-directory");
                    fs::create_dir(&denied).unwrap();
                    fs::create_dir(&replacement).unwrap();
                    c.handle(
                        json!({"op":"configure_scopes","agent_id":"agent-1", "scopes":[
                            FilesystemScope {path:root.clone(), access:FilesystemAccess::Write},
                            FilesystemScope {path:denied.clone(), access:FilesystemAccess::Deny}
                        ]}),
                    )
                    .unwrap();
                    fs::remove_dir(&denied).unwrap();
                    std::os::unix::fs::symlink(replacement, &denied).unwrap();
                }
                _ => unreachable!(),
            }
            let request = make_dependency_check_recoverable(&mut c);
            let run_dir = c.workspace.run_dir.clone();
            let project = c.workspace.original.clone();
            drop(c);
            if matches!(
                change,
                "receipt_scope_missing" | "receipt_hash" | "receipt_id" | "malformed_declaration"
            ) {
                let path = run_dir.join("claude.json");
                let mut saved = read_state(&path).unwrap();
                let candidate = &mut saved["candidate"][1];
                match change {
                    "receipt_scope_missing" => {
                        candidate["shared_checks"][0]
                            .as_object_mut()
                            .unwrap()
                            .remove("scope");
                    }
                    "receipt_hash" => {
                        candidate["shared_checks"][0]["files"]["node_modules/three/build/three.core.js"]
                            ["sha256"] = json!("0".repeat(64))
                    }
                    "receipt_id" => candidate["shared_checks"][0]["receipt_id"] = json!(999999),
                    "malformed_declaration" => {
                        candidate["declaration"]["shared_checks"] = json!("not an array")
                    }
                    _ => unreachable!(),
                }
                atomic_json(&path, &saved).unwrap();
            }
            let result = recover_at(&run_dir, &request).unwrap();
            let delivered = matches!(change, "unchanged" | "unrelated");
            if delivered {
                assert_dependency_check_delivered(&result);
            } else {
                assert_eq!(result["status"], "recovery_required", "{change}: {result}");
                if change == "saved_policy_path_replaced" {
                    assert!(
                        result["reason"]
                            .as_str()
                            .unwrap()
                            .contains("permissions resolve differently")
                    );
                }
                let recovery = Path::new(result["recovery"]["recovery"].as_str().unwrap());
                let exported =
                    workspace::export_recovery(recovery, &temp.path().join("exported"), 1).unwrap();
                assert_eq!(
                    fs::read_to_string(exported.files.join("source.txt")).unwrap(),
                    "finished\n"
                );
                assert_eq!(
                    fs::read_to_string(exported.files.join(".cache/requested.txt")).unwrap(),
                    "requested artifact\n"
                );
            }
            assert_eq!(
                fs::read_to_string(project.join("source.txt")).unwrap(),
                if delivered {
                    "finished\n"
                } else {
                    "original\n"
                },
                "{change}"
            );
            assert_eq!(project.join(".cache/requested.txt").exists(), delivered);
            assert!(!project.join("node_modules").exists());
            for name in ["baseline", "worker-1", "worker-2"] {
                assert!(!run_dir.join("workspace").join(name).exists());
            }
            let repeated = recover_at(&run_dir, &request).unwrap();
            assert_eq!(repeated["already_finished"], true);
            assert_eq!(repeated["status"], result["status"]);
        }
    }

    #[test]
    fn peer_receipt_recovery_uses_winning_worker_inputs_and_permissions() {
        for change in [
            "unchanged",
            "consumer_input_changed",
            "consumer_read_denied",
        ] {
            let (temp, mut c) = controller();
            let author = c.workspace.workers[0].clone();
            let consumer = c.workspace.workers[1].clone();
            let dependency = "node_modules/fixture/input.txt";
            for root in [&author, &consumer] {
                fs::write(root.join("source.txt"), "consumer result\n").unwrap();
                fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
                fs::create_dir_all(root.join("node_modules/fixture")).unwrap();
                fs::write(root.join(dependency), "checked input\n").unwrap();
            }
            let snapshot = tool(
                &mut c,
                1,
                "begin-peer-check",
                "delm_check_begin",
                json!({"idempotency_key":"begin-peer-check", "summary":"Check shared source and dependency",
                    "paths":["source.txt", dependency]}),
            );
            c.handle(
                json!({"op":"command_start","agent_id":"agent-1","turn_id":"turn-1",
                "call_id":"peer-check","command":"node check.mjs","cwd":author}),
            )
            .unwrap();
            c.handle(
                json!({"op":"command_end","agent_id":"agent-1","call_id":"peer-check",
                "result_ref":"peer-check-result","is_error":false,"interrupted":false,
                "background_task_id":null,"timed_out":false}),
            )
            .unwrap();
            let receipt = tool(
                &mut c,
                1,
                "finish-peer-check",
                "delm_check_finish",
                json!({"idempotency_key":"finish-peer-check", "snapshot_id":snapshot["result"]["snapshot_id"],
                    "command_id":"peer-check"}),
            )["result"]
                .clone();
            assert_eq!(receipt["worker"], 1);
            assert_eq!(receipt["reusable"], true);
            tool(
                &mut c,
                2,
                "complete-with-peer-check",
                "delm_complete",
                json!({"idempotency_key":"complete-with-peer-check", "expected_revision":1,
                    "outcome":"complete", "summary":"Reused peer check", "checks":[],
                    "shared_checks":[receipt["receipt_id"]], "artifacts":[]}),
            );
            let result = c
                .handle(
                    json!({"op":"turn_end","agent_id":"agent-2","turn_id":"turn-2",
                "reason":"completed","answer":"Implemented with the peer's checked inputs."}),
                )
                .unwrap();
            assert_eq!(result["actions"][0]["type"], "candidate");
            assert_eq!(result["actions"][0]["agent_id"], "agent-2");

            // The receipt is historical evidence. Recovery must inspect the
            // winner's matching inputs, not the author's later files or policy.
            fs::write(author.join("source.txt"), "author later result\n").unwrap();
            fs::write(author.join(dependency), "author changed input\n").unwrap();
            c.handle(json!({"op":"configure_scopes","agent_id":"agent-1","scopes":[]}))
                .unwrap();
            match change {
                "unchanged" => {}
                "consumer_input_changed" => {
                    fs::write(consumer.join(dependency), "consumer changed input\n").unwrap();
                }
                "consumer_read_denied" => {
                    c.handle(json!({"op":"configure_scopes","agent_id":"agent-2","scopes":[]}))
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let mut request = make_dependency_check_recoverable(&mut c);
            request["agents"] = json!([
                {"id":"agent-1","status":"killed"},
                {"id":"agent-2","status":"completed"}
            ]);
            let run_dir = c.workspace.run_dir.clone();
            let project = c.workspace.original.clone();
            let saved = read_state(&run_dir.join("claude.json")).unwrap();
            assert_eq!(saved["candidate"][0], 1);
            assert_eq!(saved["candidate"][1]["shared_checks"], json!([receipt]));
            assert_eq!(saved["candidate"][1]["checks"], json!([]));
            assert_eq!(saved["worker_scopes"][0], json!([]));
            if change == "consumer_read_denied" {
                assert_eq!(saved["worker_scopes"][1], json!([]));
            } else {
                assert_eq!(saved["worker_scopes"][1][0]["path"], json!(consumer));
            }
            assert!(!run_dir.join("workspace/delivery/result.json").exists());
            drop(c);

            let result = recover_at(&run_dir, &request).unwrap();
            let delivered = change == "unchanged";
            if delivered {
                assert_dependency_check_delivered(&result);
                assert_eq!(result["shared_checks"], json!([receipt]));
                assert_eq!(result["checks"], json!([]));
            } else {
                assert_eq!(result["status"], "recovery_required", "{change}: {result}");
                let expected = if change == "consumer_input_changed" {
                    "shared check input changed"
                } else {
                    "worker policy forbids"
                };
                assert!(
                    result["reason"].as_str().unwrap().contains(expected),
                    "{change}: {result}"
                );
                let recovery = Path::new(result["recovery"]["recovery"].as_str().unwrap());
                let exported =
                    workspace::export_recovery(recovery, &temp.path().join("consumer-export"), 2)
                        .unwrap();
                assert_eq!(
                    fs::read_to_string(exported.files.join("source.txt")).unwrap(),
                    "consumer result\n"
                );
            }
            assert_eq!(
                fs::read_to_string(project.join("source.txt")).unwrap(),
                if delivered {
                    "consumer result\n"
                } else {
                    "original\n"
                },
                "{change}"
            );
            assert!(!project.join("node_modules").exists());
            for name in ["baseline", "worker-1", "worker-2"] {
                assert!(
                    !run_dir.join("workspace").join(name).exists(),
                    "{change}: {name}"
                );
            }
        }
    }

    #[test]
    fn dependency_check_recovery_replays_finished_delivery_without_removed_inputs_or_user_overwrites()
     {
        let (_temp, mut c) = controller();
        complete_with_dependency_check(&mut c);
        let request = make_dependency_check_recoverable(&mut c);
        let run_dir = c.workspace.run_dir.clone();
        let path = run_dir.join("claude.json");
        let before_final_persist = fs::read(&path).unwrap();
        let project = c.workspace.original.clone();
        assert_dependency_check_delivered(&settle(&mut c).unwrap()["actions"][0]);
        assert!(c.workspace.workers.iter().all(|root| !root.exists()));
        // Simulate loss of the final controller write after durable delivery
        // and cleanup, with a subsequent user edit in the original project.
        fs::write(&path, before_final_persist).unwrap();
        fs::write(project.join("source.txt"), "later user edit\n").unwrap();
        fs::write(
            project.join(".cache/requested.txt"),
            "later artifact edit\n",
        )
        .unwrap();
        drop(c);
        let result = recover_at(&run_dir, &request).unwrap();
        assert_dependency_check_delivered(&result);
        assert_eq!(
            fs::read_to_string(project.join("source.txt")).unwrap(),
            "later user edit\n"
        );
        assert_eq!(
            fs::read_to_string(project.join(".cache/requested.txt")).unwrap(),
            "later artifact edit\n"
        );
        assert!(!project.join("node_modules").exists());
        let repeated = recover_at(&run_dir, &request).unwrap();
        assert_eq!(repeated["already_finished"], true);
        assert_dependency_check_delivered(&repeated);
    }

    #[test]
    fn dependency_check_legacy_recovery_requires_saved_permissions_only_for_shared_receipts() {
        for shared_receipt in [false, true] {
            let (temp, mut c) = controller();
            if shared_receipt {
                complete_with_dependency_check(&mut c);
            } else {
                complete(&mut c);
            }
            let request = make_dependency_check_recoverable(&mut c);
            let run_dir = c.workspace.run_dir.clone();
            let project = c.workspace.original.clone();
            drop(c);
            let path = run_dir.join("claude.json");
            let mut saved = read_state(&path).unwrap();
            saved.as_object_mut().unwrap().remove("worker_scopes");
            atomic_json(&path, &saved).unwrap();
            let result = recover_at(&run_dir, &request).unwrap();
            if shared_receipt {
                assert_eq!(result["status"], "recovery_required");
                let reason = result["reason"].as_str().unwrap();
                assert!(
                    reason.contains("lacks native check-input permissions"),
                    "{reason}"
                );
                assert!(reason.contains("preserved for recovery"), "{reason}");
                assert_eq!(
                    fs::read_to_string(project.join("source.txt")).unwrap(),
                    "original\n"
                );
                let recovery = Path::new(result["recovery"]["recovery"].as_str().unwrap());
                let exported =
                    workspace::export_recovery(recovery, &temp.path().join("exported"), 1).unwrap();
                assert_eq!(
                    fs::read_to_string(exported.files.join("source.txt")).unwrap(),
                    "finished\n"
                );
                assert_eq!(
                    fs::read_to_string(exported.files.join(".cache/requested.txt")).unwrap(),
                    "requested artifact\n"
                );
            } else {
                assert_eq!(result["status"], "complete");
                assert_eq!(
                    fs::read_to_string(project.join("source.txt")).unwrap(),
                    "finished\n"
                );
            }
            assert!(!project.join("node_modules").exists());
            for name in ["baseline", "worker-1", "worker-2"] {
                assert!(!run_dir.join("workspace").join(name).exists());
            }
        }
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
        let fence = c.finalization.clone().unwrap();
        c.handle(
            json!({"op":"begin_settle","generation":fence.generation,"revision":fence.revision}),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(c.workspace.original.join("source.txt")).unwrap(),
            "original\n"
        );
        assert!(c.handle(json!({"op":"settle","generation":fence.generation,"revision":fence.revision,"agents":[{"id":"agent-1","status":"completed"}],"background_tasks_stopped":true})).is_err());
        assert!(c.handle(json!({"op":"settle","generation":fence.generation,"revision":fence.revision,"agents":[{"id":"agent-1","status":"completed"},{"id":"agent-2","status":"running"}],"background_tasks_stopped":true})).is_err());
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
    fn a_new_update_invalidates_the_old_finalizer_before_shutdown_admission() {
        let (_temp, mut c) = controller();
        complete(&mut c);
        let old = c.finalization.clone().unwrap();
        let update = c
            .handle(json!({"op":"update","text":"Add another requirement"}))
            .unwrap();
        assert_eq!(update["revision"], 2);
        assert!(c.candidate.is_none());
        assert!(c.finalization.is_none());
        assert!(
            c.handle(
                json!({"op":"begin_settle","generation":old.generation,"revision":old.revision})
            )
            .is_err()
        );
        assert!(!c.stop_requested);
        assert!(c.workspace.workers.iter().all(|root| root.exists()));
    }

    #[test]
    fn closed_admission_rejects_updates_but_failed_shutdown_retains_retryable_delivery() {
        let (_temp, mut c) = controller();
        complete(&mut c);
        let fence = c.finalization.clone().unwrap();
        c.handle(
            json!({"op":"begin_settle","generation":fence.generation,"revision":fence.revision}),
        )
        .unwrap();
        assert!(c.handle(json!({"op":"update","text":"Too late"})).is_err());
        assert_eq!(c.revision, 1);
        assert!(!c.task.contains("Too late"));
        c.handle(json!({"op":"settlement_failed","generation":fence.generation,"revision":fence.revision})).unwrap();
        assert_eq!(c.status, "recovery_required");
        assert_eq!(c.finalization.as_ref().unwrap().intent, "deliver");
        assert!(!c.stop_requested);
        assert!(c.candidate.is_some());
        let result = settle(&mut c).unwrap();
        assert_eq!(result["actions"][0]["status"], "complete");
        assert_eq!(c.finalization.as_ref().unwrap().shutdown_ack, "confirmed");
        assert_eq!(c.finalization.as_ref().unwrap().attempt, 2);
    }

    #[test]
    fn explicit_cancellation_supersedes_a_selected_delivery_and_its_callbacks() {
        let (_temp, mut c) = controller();
        complete(&mut c);
        let old = c.finalization.clone().unwrap();
        c.handle(json!({"op":"begin_settle","generation":old.generation,"revision":old.revision}))
            .unwrap();
        c.handle(json!({"op":"cancel","reason":"User requested /delm-stop"}))
            .unwrap();
        assert!(c.candidate.is_none());
        assert_eq!(c.finalization.as_ref().unwrap().intent, "cancel");
        assert!(
            c.handle(
                json!({"op":"begin_settle","generation":old.generation,"revision":old.revision})
            )
            .is_err()
        );
        let result = settle(&mut c).unwrap();
        assert_eq!(result["actions"][0]["status"], "stopped");
        assert_eq!(
            fs::read_to_string(c.workspace.original.join("source.txt")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn execution_deadline_does_not_replace_selected_delivery_with_cancellation() {
        let (_temp, mut c) = controller();
        complete(&mut c);
        let generation = c.finalization.as_ref().unwrap().generation;
        for _ in 0..2 {
            c.handle(json!({"op":"interrupt","reason":"DeLM's execution allowance expired"}))
                .unwrap();
            assert!(!c.stop_requested);
            assert!(c.candidate.is_some());
            assert_eq!(c.finalization.as_ref().unwrap().generation, generation);
            assert_eq!(c.finalization.as_ref().unwrap().intent, "deliver");
        }
        assert_eq!(settle(&mut c).unwrap()["actions"][0]["status"], "complete");
        let (_temp, mut unfinished) = controller();
        let result = unfinished
            .handle(json!({"op":"interrupt","reason":"DeLM's execution allowance expired"}))
            .unwrap();
        assert!(unfinished.stop_requested);
        assert_eq!(result["actions"][0]["finalization"]["intent"], "cancel");
        assert_eq!(result["actions"][0]["finalization"]["reason"], "deadline");
    }

    #[test]
    fn interrupted_selected_candidate_is_revalidated_before_authenticated_recovery_delivery() {
        for changed in [false, true] {
            let (_temp, mut c) = controller();
            complete(&mut c);
            c.handle(json!({"op":"interrupt","reason":"The native Claude host exited"}))
                .unwrap();
            assert!(c.candidate.is_some());
            assert!(!c.stop_requested);
            let mut process = std::process::Command::new("/bin/sleep")
                .arg("0.01")
                .spawn()
                .unwrap();
            c.runtime = ProcessIdentity::capture(process.id()).unwrap();
            process.wait().unwrap();
            let token = "recovery-delivery-token";
            c.token_digest = Some(format!("{:x}", Sha256::digest(token)));
            if changed {
                fs::write(c.workspace.workers[0].join("source.txt"), "later change\n").unwrap();
            }
            c.persist().unwrap();
            let run_dir = c.workspace.run_dir.clone();
            let project = c.workspace.original.clone();
            drop(c);
            let request = json!({"intent":"auto","token":token,"session_id":"session",
                "agents":[{"id":"agent-1","status":"completed"},{"id":"agent-2","status":"killed"}],"background_tasks_stopped":true});
            let result = recover_at(&run_dir, &request).unwrap();
            assert_eq!(
                result["status"],
                if changed {
                    "recovery_required"
                } else {
                    "complete"
                }
            );
            assert_eq!(
                fs::read_to_string(project.join("source.txt")).unwrap(),
                if changed { "original\n" } else { "finished\n" }
            );
            let repeated = recover_at(&run_dir, &request).unwrap();
            assert_eq!(repeated["already_finished"], true);
            assert_eq!(repeated["status"], result["status"]);
        }
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
    fn cancelled_candidate_retains_declared_outputs_even_under_ignored_cache_paths() {
        let (temp, mut c) = controller();
        fs::write(c.workspace.workers[0].join(".gitignore"), ".cache/\n").unwrap();
        fs::create_dir(c.workspace.workers[0].join(".cache")).unwrap();
        fs::write(
            c.workspace.workers[0].join(".cache/requested.pdf"),
            "requested output",
        )
        .unwrap();
        tool(
            &mut c,
            1,
            "complete-artifact",
            "delm_complete",
            json!({
                "idempotency_key":"complete-artifact", "expected_revision":1, "outcome":"complete",
                "summary":"Created requested output", "checks":[], "artifacts":[".cache/requested.pdf"]
            }),
        );
        c.handle(json!({"op":"turn_end","agent_id":"agent-1","turn_id":"turn-1","reason":"completed","answer":"Created the requested output."})).unwrap();
        assert_eq!(c.retained_artifacts, [".cache/requested.pdf"]);
        c.handle(json!({"op":"cancel","reason":"User requested /delm-stop"}))
            .unwrap();
        assert!(c.candidate.is_none());
        let result = settle(&mut c).unwrap();
        let recovery = Path::new(
            result["actions"][0]["recovery"]["recovery"]
                .as_str()
                .unwrap(),
        );
        let exported =
            workspace::export_recovery(recovery, &temp.path().join("exported"), 1).unwrap();
        assert_eq!(
            fs::read_to_string(exported.files.join(".cache/requested.pdf")).unwrap(),
            "requested output"
        );
    }

    #[test]
    fn recovery_reconciles_delivery_after_cleanup_without_overwriting_later_user_edits() {
        let (_temp, mut c) = controller();
        complete(&mut c);
        let mut process = std::process::Command::new("/bin/sleep")
            .arg("0.01")
            .spawn()
            .unwrap();
        c.runtime = ProcessIdentity::capture(process.id()).unwrap();
        process.wait().unwrap();
        let token = "recovery-after-delivery-token";
        c.token_digest = Some(format!("{:x}", Sha256::digest(token)));
        c.persist().unwrap();
        let run_dir = c.workspace.run_dir.clone();
        let path = run_dir.join("claude.json");
        let before_final_persist = fs::read(&path).unwrap();
        let project = c.workspace.original.clone();
        let result = settle(&mut c).unwrap();
        assert_eq!(result["actions"][0]["status"], "complete");
        assert!(c.workspace.workers.iter().all(|path| !path.exists()));
        // State equivalent to a process loss after the delivery transaction
        // completed but before the controller's final atomic state write.
        fs::write(&path, before_final_persist).unwrap();
        fs::write(project.join("source.txt"), "later user edit\n").unwrap();
        drop(c);
        let request = json!({"intent":"auto","token":token,"session_id":"session",
            "agents":[{"id":"agent-1","status":"completed"},{"id":"agent-2","status":"killed"}],"background_tasks_stopped":true});
        let recovered = recover_at(&run_dir, &request).unwrap();
        assert_eq!(recovered["status"], "complete");
        assert_eq!(
            fs::read_to_string(project.join("source.txt")).unwrap(),
            "later user edit\n"
        );
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
    fn readiness_before_or_after_native_wait_completion_resumes_once() {
        for event_before_end in [true, false] {
            let (_temp, mut c) = controller();
            let declaration = json!({"idempotency_key":"wait", "expected_revision":1,"outcome":"waiting", "summary":"Waiting for peer", "dependency":"worker:2"});
            tool(&mut c, 1, "wait", "delm_complete", declaration.clone());
            let mut resumes = Vec::new();
            let create = |c: &mut Controller| {
                tool(
                    c,
                    2,
                    "create",
                    "delm_task_create",
                    json!({"idempotency_key":"create", "title":"Ready work", "description":"Independent work"}),
                )
            };
            if event_before_end {
                let changed = create(&mut c);
                assert!(changed["actions"].as_array().unwrap().is_empty());
                // An idempotent retry must preserve the original wait cursor.
                tool(&mut c, 1, "wait-retry", "delm_complete", declaration);
            }
            let ended = c.handle(json!({"op":"turn_end","agent_id":"agent-1","turn_id":"turn-1","reason":"completed","answer":"Waiting"})).unwrap();
            resumes.extend(ended["actions"].as_array().unwrap().iter().cloned());
            if !event_before_end {
                let changed = create(&mut c);
                resumes.extend(changed["actions"].as_array().unwrap().iter().cloned());
            }
            assert_eq!(resumes.len(), 1);
            assert_eq!(resumes[0]["type"], "resume");
            assert_eq!(resumes[0]["agent_id"], "agent-1");
            assert_eq!(resumes[0]["revision"], 1);
            assert!(c.workers[0].resume_pending);
            let changed = tool(
                &mut c,
                2,
                "create-again",
                "delm_task_create",
                json!({"idempotency_key":"create-again", "title":"More work", "description":"Another independent task"}),
            );
            assert!(changed["actions"].as_array().unwrap().is_empty());
        }
    }

    #[test]
    fn completion_intent_holds_ready_resumes_until_capture_failure_or_withdrawal() {
        for finish in ["completed", "failed", "withdrawn"] {
            let (_temp, mut c) = controller();
            tool(
                &mut c,
                1,
                "wait",
                "delm_complete",
                json!({"idempotency_key":"wait",
                "expected_revision":1,"outcome":"waiting","summary":"Waiting for peer","dependency":"worker:2"}),
            );
            c.handle(json!({"op":"turn_end","agent_id":"agent-1","turn_id":"turn-1","reason":"completed","answer":"Waiting"})).unwrap();
            let declared = tool(
                &mut c,
                2,
                "complete",
                "delm_complete",
                json!({"idempotency_key":"complete",
                "expected_revision":1,"outcome":"complete","summary":"Ready","checks":[]}),
            );
            assert_eq!(declared["actions"][0]["type"], "completion_intent");
            assert_eq!(declared["actions"][0]["pending"], true);
            assert!(c.candidate.is_none());
            assert!(!c.stop_requested);
            let change = tool(
                &mut c,
                2,
                "create",
                "delm_task_create",
                json!({"idempotency_key":"create",
                "title":"Ready work","description":"Independent ready work"}),
            );
            assert!(change["actions"].as_array().unwrap().is_empty());
            let result = if finish == "withdrawn" {
                tool(
                    &mut c,
                    2,
                    "withdraw",
                    "delm_complete",
                    json!({"idempotency_key":"withdraw",
                    "expected_revision":1,"outcome":"partial","summary":"One requirement remains","remaining":"Finish implementation","checks":[]}),
                )
            } else {
                c.handle(json!({"op":"turn_end","agent_id":"agent-2","turn_id":"turn-2","reason":finish,"answer":"Finished"})).unwrap()
            };
            let actions = result["actions"].as_array().unwrap();
            assert!(
                actions
                    .iter()
                    .any(|action| action["type"] == "completion_intent"
                        && action["pending"] == false)
            );
            if finish == "completed" {
                assert!(actions.iter().any(|action| action["type"] == "candidate"));
                assert!(!actions.iter().any(|action| action["type"] == "resume"));
            } else {
                assert!(actions.iter().any(|action| action["type"] == "resume" && action["agent_id"] == "agent-1"));
                assert!(c.candidate.is_none());
                assert!(!c.stop_requested);
            }
        }
    }

    #[test]
    fn unrelated_publication_does_not_wake_a_named_task_dependency() {
        let (_temp, mut c) = controller();
        let created = tool(
            &mut c,
            2,
            "create",
            "delm_task_create",
            json!({"idempotency_key":"create", "title":"Storage", "description":"Storage"}),
        );
        let task = created["result"]["task_id"].clone();
        let claim = tool(
            &mut c,
            2,
            "claim",
            "delm_task_claim",
            json!({"idempotency_key":"claim", "task_id":task}),
        );
        tool(
            &mut c,
            1,
            "wait",
            "delm_complete",
            json!({"idempotency_key":"wait", "expected_revision":1,"outcome":"waiting", "summary":"Waiting for storage", "dependency":format!("task:{}",task.as_i64().unwrap())}),
        );
        let ended = c.handle(json!({"op":"turn_end","agent_id":"agent-1","turn_id":"turn-1","reason":"completed","answer":"Waiting"})).unwrap();
        assert!(ended["actions"].as_array().unwrap().is_empty());
        let publication = tool(
            &mut c,
            2,
            "publish",
            "delm_publish",
            json!({"idempotency_key":"publish", "summary":"Unrelated progress", "paths":["source.txt"]}),
        );
        assert!(publication["actions"].as_array().unwrap().is_empty());
        let finished = tool(
            &mut c,
            2,
            "finish",
            "delm_task_finish",
            json!({"idempotency_key":"finish", "task_id":task,"expected_version":claim["result"]["version"],"summary":"Storage ready"}),
        );
        assert_eq!(finished["actions"][0]["agent_id"], "agent-1");
    }

    #[test]
    fn blocked_stale_and_stopping_workers_do_not_resume_for_board_changes() {
        for state in ["blocked", "stale", "stopping", "candidate"] {
            let (_temp, mut c) = controller();
            tool(
                &mut c,
                1,
                "wait",
                "delm_complete",
                json!({"idempotency_key":"wait", "expected_revision":1,"outcome":"waiting", "summary":"Waiting", "dependency":"worker:2"}),
            );
            c.handle(json!({"op":"turn_end","agent_id":"agent-1","turn_id":"turn-1","reason":"completed","answer":"Waiting"})).unwrap();
            c.board
                .call(
                    2,
                    "delm_task_create",
                    json!({"idempotency_key":"create", "title":"Ready work", "description":"Work"}),
                )
                .unwrap();
            match state {
                "blocked" => c.workers[0].blocked = true,
                "stale" => c.workers[0].revision = 0,
                "stopping" => c.stop_requested = true,
                "candidate" => {
                    let declaration = json!({"expected_revision":1,"outcome":"complete","summary":"Done","checks":[]});
                    let completion = Completion::capture_with_evidence(
                        &c.board,
                        2,
                        &declaration,
                        &c.workers[1].checks,
                        1,
                        &c.workers[1].result_policy,
                        &c.workspace.baseline_manifest,
                    )
                    .unwrap();
                    c.candidate = Some((1, completion));
                }
                _ => unreachable!(),
            }
            let mut actions = Vec::new();
            c.wake_waiting(&mut actions, "Work changed").unwrap();
            assert!(actions.is_empty(), "{state} worker resumed");
        }
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
