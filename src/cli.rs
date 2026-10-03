//! Explicit skill entry point. Nothing here runs when the plugin is merely loaded.
use crate::{
    protocol::{Event, HostCommand},
    run::state,
};
use anyhow::{Context, Result, bail, ensure};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Mutex, mpsc, oneshot, watch},
};

/// This is a monitoring lease, separate from the total execution allowance.
pub const MONITOR_LEASE_SECONDS: u64 = 60;
/// A delayed parent approval must not consume model work or execution time.
pub const CONTROL_ADMISSION_SECONDS: u64 = 5 * 60;
const MAX_TEXT_BYTES: u64 = 1024 * 1024;

#[derive(Subcommand)]
pub enum Command {
    /// Run two workers in private copies of one Git repository.
    Run {
        #[arg(long, hide = true)]
        launch_token: Option<String>,
        #[arg(long)]
        project: PathBuf,
        #[arg(long)]
        task_file: PathBuf,
        #[arg(long)]
        context_file: Option<PathBuf>,
        #[arg(long)]
        inputs_file: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long, default_value_t = crate::config::DEFAULT_RUN_SECONDS)]
        seconds: u64,
    },
    /// Read progress. Renew the monitoring lease only when explicitly requested.
    Status {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        keep_alive: bool,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=15))]
        wait_seconds: u64,
    },
    /// Send a clarification to both workers.
    Update {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        message_file: PathBuf,
        #[arg(long)]
        inputs_file: Option<PathBuf>,
    },
    /// Answer one identified worker question without answering unrelated requests.
    Answer {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        question_id: String,
        #[arg(long)]
        answers_file: PathBuf,
    },
    /// Stop this run and preserve its private projects.
    Stop {
        #[arg(long)]
        run_id: String,
    },
    #[command(hide = true)]
    Watchdog {
        #[arg(long)]
        spec: PathBuf,
    },
    #[command(hide = true)]
    LifecycleHook,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub run_id: String,
    pub status: String,
    pub message: String,
    pub request_revision: u64,
    pub path: Option<PathBuf>,
    pub partial_paths: Vec<PathBuf>,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitoring_lease_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_executable: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub questions: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

struct Monitor {
    snapshot: Snapshot,
    last_contact: Instant,
    expected_revision: u64,
    control_token: String,
    admission: Option<oneshot::Sender<()>>,
    pending_answers: std::collections::HashSet<String>,
    binding: Option<crate::lifecycle::Binding>,
}

impl Monitor {
    fn contact_deadline(&self) -> Option<Duration> {
        if self.admission.is_some() {
            Some(Duration::from_secs(CONTROL_ADMISSION_SECONDS))
        } else if self.binding.is_none() {
            Some(Duration::from_secs(MONITOR_LEASE_SECONDS))
        } else {
            None
        }
    }

    fn apply_event(&mut self, event: &Event) -> Option<HostCommand> {
        self.snapshot.message.clone_from(&event.message);
        self.snapshot.updated_at = state::now();
        if let Some(revision) = event.request_revision {
            self.snapshot.request_revision = self.snapshot.request_revision.max(revision);
            self.expected_revision = self.expected_revision.max(revision);
        }
        let answered = matches!(event.kind.as_str(), "answer_applied" | "answer_rejected")
            && event
                .id
                .as_ref()
                .is_some_and(|id| self.pending_answers.remove(id));
        if answered && event.kind == "answer_rejected" {
            self.expected_revision = self
                .expected_revision
                .saturating_sub(1)
                .max(self.snapshot.request_revision);
        }
        let accepting = event.kind == "ready"
            && event.request_revision == Some(self.expected_revision)
            && self.pending_answers.is_empty()
            && self.snapshot.status != "stopping"
            && !terminal(&self.snapshot.status);
        self.snapshot.status = match event.kind.as_str() {
            "result" => "complete",
            "error" => "error",
            "stopped" => "stopped",
            "awaiting_control" if self.admission.is_some() => "awaiting_control",
            "ready" if accepting => "finishing",
            "started" if self.snapshot.status != "stopping" && !terminal(&self.snapshot.status) => {
                "running"
            }
            "answer_applied"
                if answered
                    && self.snapshot.status != "stopping"
                    && !terminal(&self.snapshot.status) =>
            {
                "running"
            }
            _ => &self.snapshot.status,
        }
        .into();
        if event.path.is_some() {
            self.snapshot.path.clone_from(&event.path);
        }
        if event.kind == "question"
            && let (Some(id), Some(details)) = (&event.id, &event.details)
        {
            self.snapshot.questions.insert(id.clone(), details.clone());
        }
        if event.kind == "question_resolved"
            && let Some(id) = &event.id
        {
            self.snapshot.questions.remove(id);
        }
        if terminal(&self.snapshot.status) {
            self.snapshot.questions.clear();
            self.pending_answers.clear();
        }
        if event.kind == "result" {
            self.snapshot.result.clone_from(&event.details);
        }
        if !event.partial_paths.is_empty() {
            self.snapshot.partial_paths.clone_from(&event.partial_paths);
        }
        accepting.then_some(HostCommand::AcceptResult {
            request_revision: self.expected_revision,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Control {
    Lifecycle {
        signal: crate::lifecycle::Signal,
    },
    Status {
        #[serde(default)]
        keep_alive: bool,
        #[serde(default)]
        wait_seconds: u64,
    },
    Update {
        text: String,
        #[serde(default)]
        attachments: Vec<serde_json::Value>,
    },
    Answer {
        id: String,
        answers: std::collections::BTreeMap<String, Vec<String>>,
    },
    Stop,
}

#[derive(Serialize, Deserialize)]
struct AuthenticatedControl {
    token: String,
    #[serde(flatten)]
    command: Control,
}

pub async fn execute(command: Command) -> Result<()> {
    match command {
        Command::LifecycleHook => native_hook().await,
        Command::Watchdog { spec } => crate::supervisor::watchdog(&spec),
        Command::Run {
            launch_token,
            project,
            task_file,
            context_file,
            inputs_file,
            model,
            effort,
            seconds,
        } => {
            let binding = launch_token.as_deref().map(crate::lifecycle::consume_launch).transpose().context("Native launch was not confirmed. Trust DeLM in /hooks and restart Codex before invoking the skill again")?;
            ensure!(
                binding.is_some() || std::env::var_os("CODEX_THREAD_ID").is_none(),
                "DeLM's native launch hook is not active. Review and trust DeLM in /hooks, restart Codex, and invoke $delm:run again. No worker was started."
            );
            let task = read_text(&task_file)?;
            ensure!(!task.trim().is_empty(), "The task is empty");
            let context = context_file
                .as_deref()
                .map(read_text)
                .transpose()?
                .unwrap_or_default();
            let project = project.canonicalize().context("Project is unavailable")?;
            ensure!(
                !control_root()?.starts_with(&project),
                "Select a repository that does not contain DeLM control storage"
            );
            let mut request =
                crate::workers::stock_request(project, task, context, model, effort, seconds)
                    .await?;
            request.attachments = read_inputs(inputs_file.as_deref())?;
            if let Some(binding) = &binding {
                crate::workers::verify_lifecycle_hooks(&request, binding).await?;
            }
            println!(
                "{}",
                json!({"type":"settings", "model":request.model,
                "reasoning_effort":request.reasoning_effort,
                "model_selection_source":request.auth_settings["model_selection_source"],
                "control_executable":std::env::current_exe()?,
                "message":"DeLM will use your installed Codex and native account. Both workers have network access and private development environments."})
            );
            run(request, binding).await
        }
        Command::Status {
            run_id,
            keep_alive,
            wait_seconds,
        } => {
            let value = control(
                &run_id,
                Control::Status {
                    keep_alive,
                    wait_seconds,
                },
            )
            .await?;
            println!("{}", serde_json::to_string(&value)?);
            Ok(())
        }
        Command::Update {
            run_id,
            message_file,
            inputs_file,
        } => {
            let text = read_text(&message_file)?;
            ensure!(!text.trim().is_empty(), "The update is empty");
            println!(
                "{}",
                serde_json::to_string(
                    &control(
                        &run_id,
                        Control::Update {
                            text,
                            attachments: read_inputs(inputs_file.as_deref())?
                        }
                    )
                    .await?
                )?
            );
            Ok(())
        }
        Command::Answer {
            run_id,
            question_id,
            answers_file,
        } => {
            let answers = serde_json::from_str(&read_text(&answers_file)?)
                .context("Answers must map each question ID to an array of answer strings")?;
            println!(
                "{}",
                serde_json::to_string(
                    &control(
                        &run_id,
                        Control::Answer {
                            id: question_id,
                            answers
                        }
                    )
                    .await?
                )?
            );
            Ok(())
        }
        Command::Stop { run_id } => {
            println!(
                "{}",
                serde_json::to_string(&control(&run_id, Control::Stop).await?)?
            );
            Ok(())
        }
    }
}

async fn native_hook() -> Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_TEXT_BYTES,
        "Native hook input exceeds its limit"
    );
    let input = serde_json::from_slice(&bytes).context("Invalid native hook input")?;
    let action = crate::lifecycle::prepare_hook(input, &std::env::current_exe()?)?;
    if let Some(delivery) = action.delivery {
        // Native Interrupt has a short deadline. The control request merely
        // queues cancellation; the retained runtime owns bounded shutdown.
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            control(
                &delivery.run_id,
                Control::Lifecycle {
                    signal: delivery.signal,
                },
            ),
        )
        .await;
    }
    if let Some(output) = action.output {
        println!("{}", serde_json::to_string(&output)?);
    }
    Ok(())
}

fn read_inputs(path: Option<&Path>) -> Result<Vec<serde_json::Value>> {
    path.map(|path| crate::inputs::parse_manifest(&read_text(path)?))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn read_text(path: &Path) -> Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(file.metadata()?.is_file(), "Expected a regular text file");
    let mut text = String::new();
    file.take(MAX_TEXT_BYTES + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= MAX_TEXT_BYTES,
        "Task or context exceeds 1 MiB; supply a concise brief"
    );
    Ok(text)
}

// AF_UNIX path limits are short on macOS. Keep sockets in a short, owner-only
// directory while durable results remain in Application Support.
fn control_root() -> Result<PathBuf> {
    Ok(Path::new("/tmp")
        .canonicalize()?
        .join(format!("delm-{}", unsafe { libc::getuid() })))
}
fn socket_path(id: &str, create: bool) -> Result<PathBuf> {
    let id = uuid::Uuid::parse_str(id).context("Invalid run identity")?;
    let root = control_root()?;
    if create {
        match fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = fs::symlink_metadata(&root)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::getuid() }
            && metadata.mode() & 0o077 == 0,
        "DeLM control storage must be a private directory owned by this account"
    );
    Ok(root.join(format!("{id}.sock")))
}

struct SocketGuard {
    path: PathBuf,
    inode: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

async fn control(id: &str, request: Control) -> Result<Snapshot> {
    let path = state::run_path(id)?;
    let connection = match socket_path(id, false) {
        Ok(socket) => UnixStream::connect(socket).await,
        Err(_) => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
    };
    let mut stream = match connection {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            let snapshot: Snapshot =
                serde_json::from_str(&read_text(&path.join("runtime-status.json"))?)?;
            ensure!(
                matches!(request, Control::Status { .. })
                    || matches!(request, Control::Stop | Control::Lifecycle { .. })
                        && terminal(&snapshot.status),
                "This run is no longer accepting commands; its saved work is preserved"
            );
            ensure!(
                terminal(&snapshot.status),
                "The runtime is not reachable. Its watchdog will stop the workers; inspect the preserved run before restarting"
            );
            return Ok(snapshot);
        }
        Err(error) => return Err(error.into()),
    };
    // Authenticate independently of native AF_UNIX connection restrictions, using
    // a secret stored outside every worker's readable paths.
    // The secret never appears in command arguments, model context or events.
    let mut message = serde_json::to_vec(&AuthenticatedControl {
        token: read_text(&path.join("control-token"))?,
        command: request,
    })?;
    message.push(b'\n');
    stream.write_all(&message).await?;
    let mut reader = BufReader::new(stream).take(2 * 1024 * 1024 + 1);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(20), reader.read_line(&mut line)).await??;
    ensure!(
        line.len() <= 2 * 1024 * 1024,
        "Control response exceeded its limit"
    );
    let value: serde_json::Value = serde_json::from_str(&line)?;
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        bail!("{error}");
    }
    Ok(serde_json::from_value(value)?)
}

fn terminal(status: &str) -> bool {
    matches!(status, "complete" | "stopped" | "error")
}

fn ensure_update_allowed(state: &Monitor) -> Result<()> {
    ensure!(
        !terminal(&state.snapshot.status),
        "The run already ended; this update was not applied"
    );
    ensure!(
        !matches!(state.snapshot.status.as_str(), "finishing" | "stopping"),
        "The run is finishing or stopping; this update was not applied. Continue from the retained project once it is ready"
    );
    Ok(())
}

async fn stage_update_inputs(
    attachments: Vec<serde_json::Value>,
    run: PathBuf,
    closing: &mut watch::Receiver<bool>,
) -> Result<crate::inputs::StagedInputs> {
    ensure!(
        !*closing.borrow(),
        "The run ended; this update was not applied"
    );
    let (sender, staged) = oneshot::channel();
    // File I/O cannot hold the monitor lock or the async control thread. A
    // detached capture cannot publish inputs and does not keep shutdown alive.
    std::thread::Builder::new()
        .name("delm-input-capture".into())
        .spawn(move || {
            let _ = sender.send(crate::inputs::stage(&attachments, &run));
        })?;
    tokio::select! {
        result = staged => result.context("Selected-input capture stopped")?,
        _ = closing.changed() => bail!("The run ended; this update was not applied"),
    }
}

fn publish_update(
    state: &mut Monitor,
    commands: &mpsc::Sender<HostCommand>,
    text: String,
    staged: crate::inputs::StagedInputs,
    expected_revision: u64,
) -> Result<()> {
    ensure_update_allowed(state)?;
    ensure!(
        state.expected_revision == expected_revision,
        "The request changed while inputs were captured; this update was not applied. Retry with the current request."
    );
    let permit = commands
        .try_reserve()
        .context("The run could not accept this update")?;
    let attachments = staged.publish()?;
    permit.send(HostCommand::Message { text, attachments });
    state.expected_revision += 1;
    if state.admission.is_none() {
        state.last_contact = Instant::now();
    }
    state.snapshot.message = "Your update is queued for both workers.".into();
    Ok(())
}

fn apply_lifecycle_signal(
    state: &mut Monitor,
    signal: &crate::lifecycle::Signal,
    commands: &mpsc::Sender<HostCommand>,
    cancel: &watch::Sender<bool>,
) -> Result<()> {
    let binding = state
        .binding
        .as_ref()
        .context("This run has no native lifecycle binding")?;
    // A newer user turn or another acknowledged delivery can retire this
    // signal after polling. That is normal, not a reason to cancel the run.
    if !binding.accepts(signal)? {
        return Ok(());
    }
    if !terminal(&state.snapshot.status)
        && (signal.event != "Stop" || state.snapshot.questions.is_empty())
    {
        cancel.send(true).ok();
        commands.try_send(HostCommand::Stop).ok();
        state.snapshot.status = "stopping".into();
        state.snapshot.message = "The invoking Codex conversation ended or was interrupted. Preserving the workers' projects.".into();
    }
    // Clear only this exact delivery. A concurrent Interrupt must survive an
    // acknowledgment of an earlier Stop, including while a question is open.
    binding.acknowledge_signal(signal)?;
    Ok(())
}

fn poll_lifecycle(
    state: &mut Monitor,
    commands: &mpsc::Sender<HostCommand>,
    cancel: &watch::Sender<bool>,
) -> Result<()> {
    if let Some(binding) = &state.binding {
        binding.check_resources()?;
        if let Some(signal) = binding.pending_signal()? {
            apply_lifecycle_signal(state, &signal, commands, cancel)?;
        }
    }
    Ok(())
}

async fn handle_control(
    stream: UnixStream,
    monitor: Arc<Mutex<Monitor>>,
    commands: mpsc::Sender<HostCommand>,
    cancel: watch::Sender<bool>,
    mut closing: watch::Receiver<bool>,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        BufReader::new(read)
            .take(MAX_TEXT_BYTES + 4096)
            .read_until(b'\n', &mut line),
    )
    .await??;
    let reply = async {
        let request: AuthenticatedControl = serde_json::from_slice(&line)?;
        let mut state = monitor.lock().await;
        ensure!(
            token_matches(&state.control_token, &request.token),
            "Control authentication failed"
        );
        let command = request.command;
        ensure!(
            !terminal(&state.snapshot.status)
                || matches!(command, Control::Status { .. } | Control::Stop | Control::Lifecycle { .. }),
            "The run already ended; this update was not applied"
        );
        ensure!(
            !matches!(state.snapshot.status.as_str(), "finishing" | "stopping")
                || !matches!(command, Control::Update { .. } | Control::Answer { .. }),
            "The run is finishing or stopping; this update was not applied. Continue from the retained project once it is ready"
        );
        let wait = match command {
            Control::Lifecycle { signal } => {
                apply_lifecycle_signal(&mut state, &signal, &commands, &cancel)?;
                0
            }
            Control::Status {
                keep_alive,
                wait_seconds,
            } => {
                ensure!(wait_seconds <= 15, "Status wait exceeds 15 seconds");
                if keep_alive {
                    if let Some(binding) = &state.binding {
                        if state.admission.is_some() { binding.ensure_admissible()?; }
                        else { binding.check_resources()?; }
                    }
                    state.last_contact = Instant::now();
                    if let Some(admission) = state.admission.take() {
                        admission.send(()).ok();
                        if state.snapshot.status == "awaiting_control" {
                            state.snapshot.status = "preparing".into();
                            state.snapshot.message =
                                "Control access confirmed. Starting workers.".into();
                        }
                    }
                }
                wait_seconds
            }
            Control::Update { text, attachments } => {
                ensure!(
                    !text.trim().is_empty() && text.len() as u64 <= MAX_TEXT_BYTES,
                    "Invalid update text"
                );
                let expected_revision = state.expected_revision;
                let staged = if attachments.is_empty() {
                    crate::inputs::stage(&attachments, Path::new(""))?
                } else {
                    let run = state::run_path(&state.snapshot.run_id)?;
                    drop(state);
                    let staged = stage_update_inputs(attachments, run, &mut closing).await?;
                    state = monitor.lock().await;
                    staged
                };
                publish_update(&mut state, &commands, text, staged, expected_revision)?;
                0
            }
            Control::Answer { id, answers } => {
                ensure!(!state.pending_answers.contains(&id), "An answer to this question is already queued");
                let question = state.snapshot.questions.get(&id).context("This question is no longer pending")?;
                crate::run::questions::validate_answers(&question["questions"], &answers)?;
                commands.try_send(HostCommand::Answer { id: id.clone(), answers })
                    .context("The run could not accept this answer")?;
                state.pending_answers.insert(id.clone());
                // Reserve one accepted revision without changing the public
                // snapshot; a matching rejection releases this reservation.
                state.expected_revision += 1;
                state.snapshot.questions.remove(&id);
                state.last_contact = Instant::now();
                state.snapshot.message = "Your answer is queued for the identified question and both workers.".into();
                0
            }
            Control::Stop => {
                if !terminal(&state.snapshot.status) {
                    cancel.send(true).ok();
                    commands.try_send(HostCommand::Stop).ok();
                    state.snapshot.status = "stopping".into();
                    state.snapshot.message =
                        "Stopping workers and preserving their projects.".into();
                }
                0
            }
        };
        drop(state);
        if wait > 0 && !*closing.borrow() {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(wait)) => {},
                _ = closing.changed() => {},
            }
        }
        Ok::<_, anyhow::Error>(serde_json::to_value(&monitor.lock().await.snapshot)?)
    }
    .await;
    let value = match reply {
        Ok(value) => value,
        Err(error) => json!({"error":format!("{error:#}")}),
    };
    let mut encoded = serde_json::to_vec(&value)?;
    encoded.push(b'\n');
    tokio::time::timeout(Duration::from_secs(5), write.write_all(&encoded)).await??;
    Ok(())
}

async fn run(
    request: crate::protocol::StartRequest,
    binding: Option<crate::lifecycle::Binding>,
) -> Result<()> {
    let _output_guard = NonblockingOutput::new()?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let (commands, input) = mpsc::channel(64);
    let (output, mut events) = mpsc::unbounded_channel();
    let (cancel, signal) = watch::channel(false);
    let (closing, closed) = watch::channel(false);
    let (admission, admitted) = oneshot::channel();
    commands.send(HostCommand::Start(request)).await?;
    let mut runtime = tokio::spawn(crate::run::serve_controlled(
        input, output, signal, admitted,
    ));
    let monitor = Arc::new(Mutex::new(Monitor {
        snapshot: Snapshot {
            status: "preparing".into(),
            request_revision: 1,
            monitoring_lease_seconds: binding.is_none().then_some(MONITOR_LEASE_SECONDS),
            control_executable: Some(std::env::current_exe()?),
            ..Default::default()
        },
        expected_revision: 1,
        pending_answers: Default::default(),
        binding: binding.clone(),
        last_contact: Instant::now(),
        admission: Some(admission),
        control_token: format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
    }));
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    let mut listener: Option<tokio::task::JoinHandle<()>> = None;
    let mut socket_guard = None;
    let mut saved_path = None;
    let mut done = None;
    let mut output_failed = false;
    let mut events_open = true;
    let monitoring = async {
      while events_open || done.is_none() {
        tokio::select! { biased;
            _ = interrupt.recv() => { cancel.send(true).ok(); commands.try_send(HostCommand::Stop).ok(); },
            _ = terminate.recv() => { cancel.send(true).ok(); commands.try_send(HostCommand::Stop).ok(); },
            event = events.recv(), if events_open => match event {
                Some(event) => {
                    if let Some(id) = &event.run_id
                        && saved_path.is_none() {
                            let path = state::run_path(id)?.join("runtime-status.json");
                            let mut secret = OpenOptions::new().write(true).create_new(true)
                                .mode(0o600).custom_flags(libc::O_NOFOLLOW)
                                .open(path.with_file_name("control-token"))?;
                            secret.write_all(monitor.lock().await.control_token.as_bytes())?;
                            secret.sync_all()?;
                            let socket = socket_path(id, true)?;
                            let server = UnixListener::bind(&socket)?;
                            fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
                            socket_guard = Some(SocketGuard { inode: fs::symlink_metadata(&socket)?.ino(), path: socket });
                            let server_monitor = monitor.clone(); let tx = commands.clone(); let stop = cancel.clone();
                            let mut closed = closed.clone();
                            listener = Some(tokio::spawn(async move {
                                let slots = Arc::new(tokio::sync::Semaphore::new(8));
                                let mut clients = tokio::task::JoinSet::new();
                                loop {
                                    tokio::select! { biased;
                                        _ = closed.changed() => break,
                                        _ = clients.join_next(), if !clients.is_empty() => {},
                                        connection = server.accept() => {
                                            let Ok((stream, _)) = connection else { break; };
                                            let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                                            let monitor = server_monitor.clone(); let tx = tx.clone(); let stop = stop.clone(); let closed = closed.clone();
                                            clients.spawn(async move { let _permit = permit; let _ = handle_control(stream, monitor, tx, stop, closed).await; });
                                        }
                                    }
                                }
                                // Return terminal status to pending long polls before
                                // closing their connections and exiting the runtime.
                                while clients.join_next().await.is_some() {}
                            }));
                            saved_path = Some(path);
                            monitor.lock().await.snapshot.run_id.clone_from(id);
                            if let Some(binding) = &binding { binding.register(id)?; }
                    }
                    let mut current = monitor.lock().await;
                    if event.kind == "ready" && !*cancel.borrow() {
                        poll_lifecycle(&mut current, &commands, &cancel)?;
                    }
                    if let Some(accept) = current.apply_event(&event) {
                        commands.try_send(accept)?;
                    }
                    if let Some(path) = &saved_path { state::atomic_json(path, &current.snapshot)?; }
                    drop(current);
                    if !output_failed && print_event(&event).is_err() {
                        output_failed = true; cancel.send(true).ok(); commands.try_send(HostCommand::Stop).ok();
                    }
                }
                None => { events_open = false; }
            },
            result = &mut runtime, if done.is_none() => { done = Some(result); },
            _ = interval.tick() => {
                let mut current = monitor.lock().await;
                if done.is_none() && !*cancel.borrow()
                    && let Err(error) = poll_lifecycle(&mut current, &commands, &cancel) {
                    cancel.send(true).ok(); commands.try_send(HostCommand::Stop).ok();
                    current.snapshot.status = "stopping".into();
                    current.snapshot.message = format!("Native lifecycle ownership changed: {error}. Preserving work.");
                    if let Some(path) = &saved_path { state::atomic_json(path, &current.snapshot)?; }
                }
                let awaiting_control = current.admission.is_some();
                let limit = current.contact_deadline();
                if done.is_none() && !*cancel.borrow() && limit.is_some_and(|limit| current.last_contact.elapsed() >= limit) {
                    cancel.send(true).ok(); commands.try_send(HostCommand::Stop).ok();
                    current.snapshot.status = "stopping".into();
                    current.snapshot.message = if awaiting_control {
                        "Control access was not confirmed within five minutes. No task model turn was started."
                    } else {
                        "The conversation stopped monitoring DeLM. Stopping workers and preserving their work."
                    }.into();
                    if let Some(path) = &saved_path { state::atomic_json(path, &current.snapshot)?; }
                }
            },
        }
      }
      Ok::<_, anyhow::Error>(())
    }.await;
    // A broken status file, control socket or output pipe must stop the run
    // through the same path as an explicit Stop, not abandon its task handle.
    if monitoring.is_err() || output_failed {
        cancel.send(true).ok();
        commands.try_send(HostCommand::Stop).ok();
    }
    if done.is_none() {
        done = Some(runtime.await);
    }
    let result = done.context("Runtime ended without a result")?;
    let runtime_failure = match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(error) => Some(error.into()),
    };
    let failure = monitoring.err().or(runtime_failure).or_else(|| {
        output_failed.then(|| {
            anyhow::anyhow!(
                "The conversation stopped reading DeLM output; the workers were stopped"
            )
        })
    });
    if let Some(error) = &failure {
        let mut current = monitor.lock().await;
        current.snapshot.status = "error".into();
        current.snapshot.message = format!("{error:#}");
        if let Some(path) = &saved_path {
            state::atomic_json(path, &current.snapshot)?;
        }
    }
    closing.send(true).ok();
    if let Some(listener) = listener {
        listener
            .await
            .context("DeLM control server did not finish")?;
    }
    drop(socket_guard);
    if let Some(binding) = &binding {
        binding.unregister()?;
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}

fn token_matches(expected: &str, actual: &str) -> bool {
    expected.len() == actual.len()
        && expected
            .as_bytes()
            .iter()
            .zip(actual.as_bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn print_event(event: &Event) -> Result<()> {
    let mut bytes = serde_json::to_vec(event)?;
    bytes.push(b'\n');
    let mut written = 0;
    while written < bytes.len() {
        let count = unsafe {
            libc::write(
                libc::STDOUT_FILENO,
                bytes[written..].as_ptr().cast(),
                bytes.len() - written,
            )
        };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        ensure!(count > 0, "DeLM output stopped accepting progress");
        written += count as usize;
    }
    Ok(())
}

// If Codex abandons a yielded tool without closing its output pipe, a full pipe
// must not block the async owner from observing Stop or the monitoring lease.
struct NonblockingOutput(i32);
impl NonblockingOutput {
    fn new() -> Result<Self> {
        let flags = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFL) };
        ensure!(flags >= 0, "Could not inspect DeLM output");
        ensure!(
            unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) }
                == 0,
            "Could not configure DeLM output"
        );
        Ok(Self(flags))
    }
}
impl Drop for NonblockingOutput {
    fn drop(&mut self) {
        unsafe {
            libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_monitor() -> Monitor {
        Monitor {
            snapshot: Snapshot {
                status: "running".into(),
                request_revision: 1,
                ..Default::default()
            },
            last_contact: Instant::now(),
            expected_revision: 1,
            control_token: "test-secret".into(),
            admission: None,
            pending_answers: Default::default(),
            binding: None,
        }
    }

    struct NativeTest(crate::lifecycle::Binding);
    impl NativeTest {
        fn new() -> Self {
            let binding = crate::lifecycle::tests::make_binding();
            crate::lifecycle::tests::seed(&binding, true);
            Self(binding)
        }
        fn signal(&self, event: &str, turn: &str) {
            crate::lifecycle::prepare_hook(
                crate::lifecycle::tests::input(event, &self.0.session_id, turn),
                Path::new("/ignored"),
            )
            .unwrap();
        }
    }
    impl Drop for NativeTest {
        fn drop(&mut self) {
            crate::lifecycle::tests::cleanup(&self.0);
        }
    }

    #[test]
    fn admitted_native_runs_do_not_depend_on_parent_polling() {
        let native = NativeTest::new();
        let mut monitor = running_monitor();
        assert_eq!(monitor.contact_deadline(), Some(Duration::from_secs(60)));
        monitor.binding = Some(native.0.clone());
        monitor.last_contact = Instant::now() - Duration::from_secs(120);
        assert_eq!(monitor.contact_deadline(), None);
        let (admission, _) = oneshot::channel();
        monitor.admission = Some(admission);
        assert_eq!(monitor.contact_deadline(), Some(Duration::from_secs(300)));
    }

    #[test]
    fn native_cancellation_is_checked_before_result_acceptance() {
        let native = NativeTest::new();
        native
            .0
            .register(&uuid::Uuid::new_v4().to_string())
            .unwrap();
        let mut state = running_monitor();
        state.binding = Some(native.0.clone());
        let (commands, mut input) = mpsc::channel(2);
        let (cancel, cancelled) = watch::channel(false);
        native.signal("Interrupt", &native.0.turn_id);
        poll_lifecycle(&mut state, &commands, &cancel).unwrap();
        let mut ready = Event::new("ready", "Candidate complete");
        ready.request_revision = Some(1);
        assert!(state.apply_event(&ready).is_none());
        assert!(*cancelled.borrow());
        assert!(matches!(input.try_recv().unwrap(), HostCommand::Stop));
        assert!(native.0.pending_signal().unwrap().is_none());
    }

    #[test]
    fn retired_stop_does_not_cancel_a_new_user_turn() {
        let native = NativeTest::new();
        native
            .0
            .register(&uuid::Uuid::new_v4().to_string())
            .unwrap();
        let mut state = running_monitor();
        state.binding = Some(native.0.clone());
        let (commands, _) = mpsc::channel(2);
        let (cancel, cancelled) = watch::channel(false);
        native.signal("Stop", &native.0.turn_id);
        let old = native.0.pending_signal().unwrap().unwrap();
        native.signal("UserPromptSubmit", &uuid::Uuid::new_v4().to_string());
        apply_lifecycle_signal(&mut state, &old, &commands, &cancel).unwrap();
        assert!(!*cancelled.borrow());
        let mut ready = Event::new("ready", "Candidate complete");
        ready.request_revision = Some(1);
        assert!(state.apply_event(&ready).is_some());
    }

    #[test]
    fn asking_for_input_ignores_stop_but_never_interrupt() {
        let native = NativeTest::new();
        native
            .0
            .register(&uuid::Uuid::new_v4().to_string())
            .unwrap();
        let mut state = running_monitor();
        state.binding = Some(native.0.clone());
        state.snapshot.questions.insert("pending".into(), json!({}));
        let (commands, _) = mpsc::channel(2);
        let (cancel, cancelled) = watch::channel(false);
        native.signal("Stop", &native.0.turn_id);
        poll_lifecycle(&mut state, &commands, &cancel).unwrap();
        assert!(!*cancelled.borrow());
        assert!(native.0.pending_signal().unwrap().is_none());
        native.signal("Stop", &native.0.turn_id);
        let old = native.0.pending_signal().unwrap().unwrap();
        native.signal("Interrupt", &native.0.turn_id);
        native.0.acknowledge_signal(&old).unwrap();
        assert_eq!(
            native.0.pending_signal().unwrap().unwrap().event,
            "Interrupt"
        );
        poll_lifecycle(&mut state, &commands, &cancel).unwrap();
        assert!(*cancelled.borrow());
    }

    #[tokio::test]
    async fn cancelled_native_launch_never_receives_control_admission() {
        let native = NativeTest::new();
        let mut state = running_monitor();
        let (admission, mut admitted) = oneshot::channel();
        state.admission = Some(admission);
        state.binding = Some(native.0.clone());
        let monitor = Arc::new(Mutex::new(state));
        let (commands, mut input) = mpsc::channel(2);
        let (cancel, cancelled) = watch::channel(false);
        native.signal("Interrupt", &native.0.turn_id);
        let (server, mut client) = UnixStream::pair().unwrap();
        let (_, closing) = watch::channel(false);
        let task = tokio::spawn(handle_control(
            server,
            monitor.clone(),
            commands,
            cancel,
            closing,
        ));
        client.write_all(b"{\"token\":\"test-secret\",\"type\":\"status\",\"keep_alive\":true,\"wait_seconds\":0}\n").await.unwrap();
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).await.unwrap();
        task.await.unwrap().unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&line).unwrap()["error"].is_string());
        assert!(monitor.lock().await.admission.is_some());
        assert!(matches!(
            admitted.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(input.try_recv().is_err());
        assert!(!*cancelled.borrow()); // Admission is blocked before the monitor handles cancellation.
    }

    #[test]
    fn ready_waits_for_each_answer_acknowledgment() {
        let mut monitor = running_monitor();
        monitor.pending_answers.extend(["one".into(), "two".into()]);
        monitor.expected_revision = 3;
        let mut ready = Event::new("ready", "Ready");
        ready.request_revision = Some(1);
        assert!(monitor.apply_event(&ready).is_none());
        assert_eq!(monitor.snapshot.status, "running");
        let mut resolved = Event::new("question_resolved", "No longer pending");
        resolved.id = Some("one".into());
        monitor.apply_event(&resolved);
        assert_eq!(monitor.pending_answers.len(), 2);
        let mut applied = Event::new("answer_applied", "Answer applied");
        applied.id = Some("one".into());
        applied.request_revision = Some(2);
        monitor.apply_event(&applied);
        ready.request_revision = Some(2);
        assert!(monitor.apply_event(&ready).is_none());
        assert_eq!(monitor.pending_answers.len(), 1);
        let mut rejected = Event::new("answer_rejected", "Question retired");
        rejected.id = Some("two".into());
        monitor.apply_event(&rejected);
        assert!(matches!(
            monitor.apply_event(&ready),
            Some(HostCommand::AcceptResult {
                request_revision: 2
            })
        ));
        assert_eq!(monitor.snapshot.status, "finishing");
        monitor.apply_event(&applied); // A repeated acknowledgment cannot reopen acceptance.
        assert_eq!(monitor.snapshot.status, "finishing");
        monitor.snapshot.status = "stopping".into();
        monitor.pending_answers.insert("one".into());
        monitor.apply_event(&applied);
        assert_eq!(monitor.snapshot.status, "stopping");
        assert!(monitor.apply_event(&ready).is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn staged_updates_are_not_published_after_stop_change_or_queue_failure() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("reference.txt");
        fs::write(&source, "original reference").unwrap();
        for failure in ["stopping", "complete", "changed", "queue-full", "none"] {
            let run = directory.path().join(failure);
            fs::create_dir(&run).unwrap();
            let staged =
                crate::inputs::stage(&[json!({"type":"file","path":source})], &run).unwrap();
            assert!(!run.join("attachments").exists());
            let mut monitor = running_monitor();
            let (tx, mut rx) = mpsc::channel(1);
            match failure {
                "stopping" | "complete" => monitor.snapshot.status = failure.into(),
                "changed" => monitor.expected_revision = 2,
                "queue-full" => tx.try_send(HostCommand::Stop).unwrap(),
                _ => {}
            }
            let result = publish_update(&mut monitor, &tx, "Use this input".into(), staged, 1);
            if failure == "none" {
                result.unwrap();
                assert_eq!(monitor.expected_revision, 2);
                assert!(run.join("attachments").is_dir());
                assert!(matches!(
                    rx.try_recv().unwrap(),
                    HostCommand::Message { .. }
                ));
            } else {
                assert!(result.is_err());
                assert!(!run.join("attachments").exists());
                assert_eq!(
                    monitor.expected_revision,
                    if failure == "changed" { 2 } else { 1 }
                );
            }
            assert_eq!(fs::read_to_string(&source).unwrap(), "original reference");
        }
    }

    #[tokio::test]
    async fn duplicate_answers_are_not_queued_twice() {
        let mut state = running_monitor();
        state.snapshot.questions.insert(
            "question".into(),
            json!({"questions":[{"id":"format","question":"Which format?"}]}),
        );
        let monitor = Arc::new(Mutex::new(state));
        let (tx, mut rx) = mpsc::channel(2);
        let (stop, signal) = watch::channel(false);
        for duplicate in [false, true] {
            let (server, mut client) = UnixStream::pair().unwrap();
            let task = tokio::spawn(handle_control(
                server,
                monitor.clone(),
                tx.clone(),
                stop.clone(),
                signal.clone(),
            ));
            client.write_all(b"{\"token\":\"test-secret\",\"type\":\"answer\",\"id\":\"question\",\"answers\":{\"format\":[\"SVG\"]}}\n").await.unwrap();
            let mut line = String::new();
            BufReader::new(client).read_line(&mut line).await.unwrap();
            task.await.unwrap().unwrap();
            let result: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(result["error"].is_string(), duplicate);
        }
        assert!(matches!(rx.try_recv().unwrap(), HostCommand::Answer { .. }));
        assert!(rx.try_recv().is_err());
        let monitor = monitor.lock().await;
        assert_eq!(monitor.pending_answers.len(), 1);
        assert_eq!(monitor.expected_revision, 2);
    }

    #[test]
    fn queued_update_and_answer_do_not_share_one_expected_revision() {
        let mut monitor = running_monitor();
        monitor.pending_answers.insert("answer".into());
        monitor.expected_revision = 3; // Answer and later update are both queued.
        let mut applied = Event::new("answer_applied", "Applying answer");
        applied.id = Some("answer".into());
        applied.request_revision = Some(2);
        monitor.apply_event(&applied);
        let mut ready = Event::new("ready", "Obsolete candidate");
        ready.request_revision = Some(2);
        assert!(monitor.apply_event(&ready).is_none());
        assert_eq!(monitor.snapshot.status, "running");
        assert_eq!(monitor.expected_revision, 3);
        let mut update = Event::new("status", "Applying update");
        update.request_revision = Some(3);
        monitor.apply_event(&update);
        ready.request_revision = Some(3);
        assert!(monitor.apply_event(&ready).is_some());
    }

    #[tokio::test]
    async fn updates_after_result_acceptance_or_stop_are_rejected_not_queued() {
        for status in ["finishing", "stopping"] {
            let monitor = Arc::new(Mutex::new(Monitor {
                snapshot: Snapshot {
                    status: status.into(),
                    ..Default::default()
                },
                expected_revision: 1,
                pending_answers: Default::default(),
                binding: None,
                last_contact: Instant::now(),
                control_token: "test-secret".into(),
                admission: None,
            }));
            let (tx, mut rx) = mpsc::channel(2);
            let (stop, signal) = watch::channel(false);
            let (server, mut client) = UnixStream::pair().unwrap();
            let task = tokio::spawn(handle_control(server, monitor.clone(), tx, stop, signal));
            client
                .write_all(
                    b"{\"token\":\"test-secret\",\"type\":\"update\",\"text\":\"Late change\"}\n",
                )
                .await
                .unwrap();
            let mut line = String::new();
            BufReader::new(client).read_line(&mut line).await.unwrap();
            task.await.unwrap().unwrap();
            let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert!(reply["error"].as_str().unwrap().contains("not applied"));
            assert!(rx.try_recv().is_err());
            assert_eq!(monitor.lock().await.expected_revision, 1);
        }
    }
    #[test]
    fn inputs_reject_links_and_non_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("task"), "Do the task").unwrap();
        std::os::unix::fs::symlink(dir.path().join("task"), dir.path().join("link")).unwrap();
        assert!(read_text(&dir.path().join("link")).is_err());
        assert!(read_text(dir.path()).is_err());
        assert_eq!(read_text(&dir.path().join("task")).unwrap(), "Do the task");
        assert!(socket_path("../arbitrary", false).is_err());
    }
    #[tokio::test]
    async fn update_queue_preserves_revision_and_stop_cancels() {
        let monitor = Arc::new(Mutex::new(Monitor {
            snapshot: Snapshot::default(),
            expected_revision: 1,
            pending_answers: Default::default(),
            binding: None,
            last_contact: Instant::now(),
            control_token: "test-secret".into(),
            admission: None,
        }));
        let (tx, mut rx) = mpsc::channel(2);
        let (stop, signal) = watch::channel(false);
        for (command, expected_revision) in [
            (
                Control::Update {
                    text: "New constraint".into(),
                    attachments: Vec::new(),
                },
                2,
            ),
            (Control::Stop, 2),
        ] {
            let (server, mut client) = UnixStream::pair().unwrap();
            let task = tokio::spawn(handle_control(
                server,
                monitor.clone(),
                tx.clone(),
                stop.clone(),
                signal.clone(),
            ));
            client
                .write_all(
                    format!(
                        "{}\n",
                        serde_json::to_string(&AuthenticatedControl {
                            token: "test-secret".into(),
                            command
                        })
                        .unwrap()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut line = String::new();
            BufReader::new(client).read_line(&mut line).await.unwrap();
            task.await.unwrap().unwrap();
            assert_eq!(monitor.lock().await.expected_revision, expected_revision);
            assert!(rx.recv().await.is_some());
        }
        assert!(*signal.borrow());
    }
}
