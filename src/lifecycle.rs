//! Explicit invocation capture and native ownership. No chat transcript parsing.
//!
//! A launch is bound by a one-use PreToolUse handshake, not by a directory or a
//! guessed active thread. Supported native-bound runs use events and exact owner
//! identity without a chat heartbeat. Hooks must stay enabled and trusted.
use crate::supervisor::ProcessIdentity;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const REQUIRED_EVENTS: [&str; 4] = ["preToolUse", "userPromptSubmit", "interrupt", "stop"];
const MAX_RECORD: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Binding {
    pub session_id: String,
    pub invocation_id: String,
    pub turn_id: String,
    pub owner: ProcessIdentity,
    pub executable: PathBuf,
    pub executable_hash: String,
    pub executable_identity: FileIdentity,
    pub hooks_file: PathBuf,
    pub hooks_hash: String,
    pub hooks_identity: FileIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    mode: u32,
    uid: u32,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileIdentity {
    fn capture(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_file(),
            "Lifecycle resource is not a regular file"
        );
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            links: metadata.nlink(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Registration {
    binding: Binding,
    run_id: Option<String>,
    runtime: Option<ProcessIdentity>,
    consumed: bool,
    finished: bool,
    pending: Option<Signal>,
    previous_invocations: Vec<String>,
    // All accepted turns are retained for this bounded run. A repeated older
    // UserPromptSubmit must never move ownership back to an interrupted turn.
    previous_turns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Signal {
    pub session_id: String,
    pub invocation_id: String,
    pub turn_id: Option<String>,
    pub owner: ProcessIdentity,
    pub event: String,
}

#[derive(Debug, Clone)]
pub struct Delivery {
    pub run_id: String,
    pub signal: Signal,
}

#[derive(Debug, Default)]
pub struct HookAction {
    pub output: Option<Value>,
    pub delivery: Option<Delivery>,
    pub launch: Option<CapturedInvocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapturedInvocation {
    pub session_id: String,
    pub invocation_id: String,
    pub turn_id: String,
    pub project: PathBuf,
    pub task: String,
    pub path: PathBuf,
    #[serde(default)]
    pub captured_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CaptureLaunch {
    Pending,
    Started { process: ProcessIdentity },
    Failed { message: String },
}

#[derive(Debug, Deserialize)]
pub struct HookInput {
    pub hook_event_name: String,
    pub session_id: String,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Value,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

fn validate_id(value: &str) -> Result<()> {
    uuid::Uuid::parse_str(value).context("Invalid native lifecycle identity")?;
    Ok(())
}

fn directory(path: &Path, create: bool) -> Result<bool> {
    if create {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "Native lifecycle storage is not private"
    );
    Ok(true)
}

fn root(create: bool) -> Result<Option<PathBuf>> {
    let parent = Path::new("/tmp")
        .canonicalize()?
        .join(format!("delm-{}", unsafe { libc::geteuid() }));
    if !directory(&parent, create)? {
        return Ok(None);
    }
    let path = parent.join("lifecycle");
    Ok(directory(&path, create)?.then_some(path))
}

fn read_private<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.nlink() == 1
            && metadata.mode() & 0o077 == 0,
        "Native lifecycle record is not private"
    );
    ensure!(
        metadata.len() <= MAX_RECORD,
        "Native lifecycle record exceeds its bound"
    );
    let mut data = Vec::new();
    file.take(MAX_RECORD + 1).read_to_end(&mut data)?;
    ensure!(
        data.len() as u64 <= MAX_RECORD,
        "Native lifecycle record grew beyond its bound"
    );
    Ok(serde_json::from_slice(&data)?)
}

fn write_private(path: &Path, value: &impl Serialize) -> Result<()> {
    let pending = path.with_extension(format!("{}.pending", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&pending)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        fs::rename(&pending, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(pending);
    }
    result
}

fn lock(root: &Path, session: &str) -> Result<File> {
    lock_for(root, session, Duration::from_millis(500))
}

fn lock_for(root: &Path, session: &str, budget: Duration) -> Result<File> {
    validate_id(session)?;
    let path = root.join(format!("session-{session}.lock"));
    lock_path(&path, budget)
}

fn capture_lock(root: &Path, session: &str, turn: &str) -> Result<File> {
    validate_id(session)?;
    validate_id(turn)?;
    lock_path(
        &root.join(format!("capture-{session}-{turn}.lock")),
        Duration::from_millis(500),
    )
}

fn lock_path(path: &Path, budget: Duration) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.nlink() == 1
            && metadata.mode() & 0o077 == 0,
        "Invalid lifecycle lock"
    );
    let until = Instant::now() + budget;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => break,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < until =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error).context("Native session lifecycle registry is busy"),
        }
    }
    Ok(file)
}

fn session_path(root: &Path, session: &str) -> PathBuf {
    root.join(format!("session-{session}.json"))
}

// shlex splits words but does not enforce literal shell syntax. Reject shell
// operators/expansion outside quotes before interpreting paths from those words.
fn literal_words(command: &str) -> Result<Vec<String>> {
    let mut quote = None;
    let mut escaped = false;
    for c in command.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if quote == Some('\'') {
            if c == '\'' {
                quote = None;
            }
            continue;
        }
        if c == '"' {
            quote = if quote == Some('"') { None } else { Some('"') };
            continue;
        }
        if c == '\'' && quote.is_none() {
            quote = Some('\'');
            continue;
        }
        let expansion = matches!(c, '$' | '`');
        let shell_operator = quote.is_none()
            && matches!(
                c,
                ';' | '&'
                    | '|'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '*'
                    | '?'
                    | '['
                    | ']'
                    | '~'
                    | '#'
                    | '\n'
                    | '\r'
            );
        ensure!(
            !(expansion || shell_operator),
            "DeLM native launch must be one literal command without shell expansion"
        );
    }
    shlex::split(command).context("Malformed DeLM launch quoting")
}

fn project_from_launch(tail: &str) -> Result<PathBuf> {
    let words = literal_words(tail)?;
    ensure!(
        words
            .iter()
            .filter(|word| word.as_str() == "--project")
            .count()
            == 1,
        "DeLM launch must contain one explicit --project path"
    );
    let index = words.iter().position(|word| word == "--project").unwrap();
    let project = PathBuf::from(words.get(index + 1).context("Missing --project path")?);
    ensure!(
        project.is_absolute(),
        "DeLM native launch requires an absolute project path"
    );
    let project = project.canonicalize()?;
    ensure!(project.is_dir(), "DeLM project must be a directory");
    let control = Path::new("/tmp")
        .canonicalize()?
        .join(format!("delm-{}", unsafe { libc::geteuid() }));
    ensure!(
        !physically_contains(&project, &control)?,
        "The project contains DeLM control storage"
    );
    let storage = PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?)
        .join("Library/Application Support/DeLM");
    ensure!(
        !physically_contains(&project, &storage)?,
        "The project contains DeLM runtime storage"
    );
    crate::run::state::check_storage_boundary(&project)?;
    Ok(project)
}

fn physically_contains(project: &Path, destination: &Path) -> Result<bool> {
    let project = fs::metadata(project)?;
    let mut existing = destination;
    while !existing.try_exists()? {
        existing = existing.parent().context("No storage ancestor")?;
    }
    let existing = existing.canonicalize()?;
    for ancestor in existing.ancestors() {
        let metadata = fs::metadata(ancestor)?;
        if metadata.dev() == project.dev() && metadata.ino() == project.ino() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn reclaim_dead_sessions(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let until = Instant::now() + Duration::from_millis(100);
    let mut inspected = 0;
    for entry in entries.flatten() {
        if Instant::now() >= until || inspected >= 64 {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(session) = name
            .strip_prefix("session-")
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        inspected += 1;
        if validate_id(session).is_err() {
            continue;
        }
        let Ok(_lock) = lock_for(root, session, Duration::ZERO) else {
            continue;
        };
        let Ok(record) = read_private::<Registration>(&entry.path()) else {
            continue;
        };
        // Never reap an unresolved identity or an active runtime. The lock inode
        // is retained so a concurrent waiter cannot acquire a second lock file.
        if record.binding.owner.is_running().is_ok_and(|alive| !alive)
            && record
                .runtime
                .is_none_or(|runtime| runtime.is_running().is_ok_and(|alive| !alive))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn digest(path: &Path) -> Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= 256 * 1024 * 1024,
        "Invalid lifecycle resource"
    );
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(
            total <= 256 * 1024 * 1024,
            "Lifecycle resource grew beyond its bound"
        );
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Canonical launch syntax deliberately avoids parsing arbitrary shell programs.
/// The skill supplies this exact quoted executable as a single `exec` command.
pub fn launch_prefix(executable: &Path) -> String {
    format!(
        "exec '{}' run ",
        executable.to_string_lossy().replace('\'', "'\\''")
    )
}

/// Parse only the fields needed to route native events. Unknown event kinds and
/// ordinary tool calls are silent; no directory is created until an explicit run.
pub fn prepare_hook(input: HookInput, executable: &Path) -> Result<HookAction> {
    if std::env::var_os("DELM_WORKER_SESSION").is_some() {
        return Ok(HookAction::default());
    }
    if input.agent_id.as_ref().is_some_and(|id| !id.is_empty()) {
        return Ok(HookAction::default());
    }
    if input.hook_event_name == "UserPromptSubmit"
        && let Some(task) = input.prompt.as_deref().and_then(explicit_task)
    {
        let captured_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
            .try_into()?;
        let project = input
            .cwd
            .as_ref()
            .context("Native invocation omitted the project")?
            .canonicalize()?;
        let turn_id = input
            .turn_id
            .as_ref()
            .context("Native invocation omitted its turn")?;
        validate_id(&input.session_id)?;
        validate_id(turn_id)?;
        let root = root(true)?.context("Missing lifecycle storage")?;
        // This lock is distinct from the session lock acquired by PreToolUse
        // below. Concurrent duplicate hooks must not create different launches.
        let _capture_lock = capture_lock(&root, &input.session_id, turn_id)?;
        // A repeated delivery reconnects to exactly the same captured invocation.
        let capture_path = root.join(format!("input-{}-{turn_id}.json", input.session_id));
        if capture_path.try_exists()? {
            let capture: CapturedInvocation = read_private(&capture_path)?;
            ensure!(
                capture.task == task && capture.project == project,
                "Repeated native invocation changed its input"
            );
            return Ok(HookAction {
                output: Some(capture_context(&capture, executable)),
                ..Default::default()
            });
        }
        let nonce = uuid::Uuid::new_v4().to_string();
        let quoted = project.to_string_lossy().replace('\'', "'\\''");
        let command = format!(
            "{}--launch-token {nonce} --project '{quoted}'",
            launch_prefix(&executable.canonicalize()?)
        );
        // Reuse the exact native owner binding; no tool approval is fabricated.
        prepare_hook(
            serde_json::from_value(
                serde_json::json!({"hook_event_name":"PreToolUse","session_id":input.session_id,
            "turn_id":turn_id,"tool_name":"Bash","tool_input":{"command":command}}),
            )?,
            executable,
        )?;
        let capture = CapturedInvocation {
            session_id: input.session_id,
            invocation_id: nonce,
            turn_id: turn_id.clone(),
            project,
            task: task.into(),
            path: capture_path,
            captured_at_ms,
        };
        write_private(&capture.path, &capture)?;
        record_capture_launch(&capture, &CaptureLaunch::Pending)?;
        return Ok(HookAction {
            output: Some(capture_context(&capture, executable)),
            launch: Some(capture),
            ..Default::default()
        });
    }
    if input.hook_event_name == "PreToolUse" {
        if !matches!(
            input.tool_name.as_deref(),
            Some("Bash" | "exec_command" | "functions.exec_command")
        ) {
            return Ok(HookAction::default());
        }
        // Stock 0.159.3 normalizes exec_command to Bash {command}; some host
        // sources expose the original exec_command {cmd}. Keep both explicit.
        let command_key = if input.tool_name.as_deref() == Some("Bash") {
            "command"
        } else {
            "cmd"
        };
        let Some(command) = input
            .tool_input
            .get(command_key)
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Ok(HookAction::default());
        };
        let executable = executable.canonicalize()?;
        let prefix = launch_prefix(&executable);
        let Some(tail) = command.strip_prefix(&prefix) else {
            return Ok(HookAction::default());
        };
        project_from_launch(tail)?;
        // 0.159.3 requires permissionDecision:allow for updatedInput. Never
        // auto-approve a native command merely to attach ownership metadata.
        // The skill supplies the invocation UUID; the hook records its binding.
        let nonce = tail
            .strip_prefix("--launch-token ")
            .and_then(|value| value.split_once(' ').map(|(nonce, _)| nonce))
            .context("DeLM launch needs --launch-token followed by a fresh UUID")?;
        validate_id(nonce)?;
        validate_id(&input.session_id)?;
        let turn_id = input
            .turn_id
            .context("Native launch hook did not supply a turn identity")?;
        validate_id(&turn_id)?;
        let hooks_file = executable
            .parent()
            .and_then(Path::parent)
            .context("Launch must use the installed plugin executable")?
            .join("hooks/hooks.json");
        let binding = Binding {
            session_id: input.session_id,
            invocation_id: nonce.into(),
            turn_id,
            owner: ProcessIdentity::capture(unsafe { libc::getppid() } as u32)?,
            executable_hash: digest(&executable)?,
            executable_identity: FileIdentity::capture(&executable)?,
            hooks_hash: digest(&hooks_file)?,
            hooks_identity: FileIdentity::capture(&hooks_file)?,
            executable,
            hooks_file,
        };
        let root = root(true)?.context("Missing lifecycle storage")?;
        reclaim_dead_sessions(&root);
        let _lock = lock(&root, &binding.session_id)?;
        let path = session_path(&root, &binding.session_id);
        let mut previous_invocations = Vec::new();
        let mut previous_turns = Vec::new();
        if path.try_exists()? {
            let old: Registration = read_private(&path)?;
            ensure!(
                old.binding.invocation_id != nonce
                    && !old.previous_invocations.iter().any(|seen| seen == nonce),
                "This invocation was already used; reconnect to its existing DeLM run"
            );
            let dead_runtime = old
                .runtime
                .is_some_and(|runtime| runtime.is_running().is_ok_and(|alive| !alive));
            let unconsumed = !old.consumed && old.runtime.is_none();
            ensure!(
                unconsumed || old.finished || dead_runtime || old.pending.is_some(),
                "This native session already has a pending or active DeLM invocation; reconnect to run {:?}",
                old.run_id
            );
            // Cancellation can precede graceful shutdown. Never admit a second
            // runtime while the prior one still exists, even if it is stopping.
            ensure!(
                old.runtime.is_none() || dead_runtime || old.finished,
                "The prior DeLM runtime is still stopping; wait for its terminal result"
            );
            ensure!(
                !old.consumed || old.binding.turn_id != binding.turn_id,
                "A native turn can own only one consumed DeLM invocation. Send a fresh user message, then invoke $delm:run again."
            );
            previous_invocations = old.previous_invocations;
            ensure!(
                previous_invocations.len() < 256,
                "Native session invocation bound reached; start a new Codex conversation"
            );
            previous_invocations.push(old.binding.invocation_id);
            previous_turns = old.previous_turns;
            if old.binding.turn_id != binding.turn_id
                && !previous_turns.contains(&old.binding.turn_id)
            {
                previous_turns.push(old.binding.turn_id);
            }
        }
        write_private(
            &path,
            &Registration {
                binding,
                run_id: None,
                runtime: None,
                consumed: false,
                finished: false,
                pending: None,
                previous_invocations,
                previous_turns,
            },
        )?;
        return Ok(HookAction::default());
    }
    // SessionEnd lacks a turn/invocation identity. Until late old-session
    // ordering is qualified, it is advisory; owner-death polling still applies.
    if !matches!(
        input.hook_event_name.as_str(),
        "UserPromptSubmit" | "Interrupt" | "Stop"
    ) {
        return Ok(HookAction::default());
    }
    if validate_id(&input.session_id).is_err() {
        return Ok(HookAction::default());
    }
    let Some(root) = root(false)? else {
        return Ok(HookAction::default());
    };
    let path = session_path(&root, &input.session_id);
    if !path.try_exists()? {
        return Ok(HookAction::default());
    }
    let _lock = lock(&root, &input.session_id)?;
    let mut registration: Registration = read_private(&path)?;
    // `exec` in the hook command makes this the exact native host process,
    // including birth identity. Another session/server cannot send this event.
    let owner = ProcessIdentity::capture(unsafe { libc::getppid() } as u32)?;
    if registration.binding.owner != owner || registration.finished {
        return Ok(HookAction::default());
    }
    if input.hook_event_name == "UserPromptSubmit" {
        let Some(turn) = input.turn_id else {
            return Ok(HookAction::default());
        };
        validate_id(&turn)?;
        if turn != registration.binding.turn_id && !registration.previous_turns.contains(&turn) {
            registration
                .previous_turns
                .push(registration.binding.turn_id.clone());
            ensure!(
                registration.previous_turns.len() <= 4096,
                "Native turn history exceeds run bound"
            );
            registration.binding.turn_id = turn;
            // A Stop on the prior completed turn may have been a question.
            // It cannot cancel a later answered turn after delivery was delayed.
            // Explicit Interrupt and pre-admission cancellation stay durable.
            if registration.run_id.is_some()
                && registration
                    .pending
                    .as_ref()
                    .is_some_and(|signal| signal.event == "Stop")
            {
                registration.pending = None;
            }
            write_private(&path, &registration)?;
        }
        return Ok(HookAction::default());
    }
    if input.turn_id.as_deref() != Some(&registration.binding.turn_id) {
        return Ok(HookAction::default());
    }
    let signal = Signal {
        session_id: input.session_id,
        invocation_id: registration.binding.invocation_id.clone(),
        turn_id: input.turn_id,
        owner,
        event: input.hook_event_name,
    };
    // Stop may be ignored while a user answer is awaited; it must never lower
    // the severity of an already accepted explicit Interrupt.
    if registration
        .pending
        .as_ref()
        .is_none_or(|pending| pending.event != "Interrupt")
    {
        registration.pending = Some(signal);
    }
    write_private(&path, &registration)?;
    Ok(HookAction {
        output: None,
        delivery: registration.run_id.map(|run_id| Delivery {
            run_id,
            signal: registration.pending.unwrap(),
        }),
        launch: None,
    })
}

fn explicit_task(prompt: &str) -> Option<&str> {
    let tail = prompt.strip_prefix("$delm:run")?;
    if !tail.starts_with(char::is_whitespace) {
        return None;
    }
    let task = tail.trim_start();
    (!task.is_empty()).then_some(task)
}

fn capture_context(capture: &CapturedInvocation, executable: &Path) -> Value {
    let quote = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"));
    serde_json::json!({"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":format!(
        "DeLM captured this explicit invocation and is starting its runtime. Do not reconstruct the task, start another run, or implement it in the parent. Monitor with: {} follow --capture {}. Read progress and relay actual worker questions/approval requests. The runtime forwards the native user input and inherited context. Follow the DeLM skill for updates and the final project handoff.",quote(executable),quote(&capture.path))}})
}

pub fn read_capture(path: &Path) -> Result<CapturedInvocation> {
    let capture: CapturedInvocation = read_private(path)?;
    ensure!(
        capture.path == path && capture.path.parent() == root(false)?.as_deref(),
        "Invalid captured invocation path"
    );
    Ok(capture)
}

pub fn record_capture_launch(capture: &CapturedInvocation, state: &CaptureLaunch) -> Result<()> {
    write_private(&capture.path.with_extension("launch.json"), state)
}

pub fn capture_launch(capture: &CapturedInvocation) -> Result<Option<CaptureLaunch>> {
    let path = capture.path.with_extension("launch.json");
    if !path.try_exists()? {
        return Ok(None);
    }
    read_private(&path).map(Some)
}

pub fn ensure_not_worker() -> Result<()> {
    ensure!(
        std::env::var_os("DELM_WORKER_SESSION").is_none(),
        "This session is already a DeLM worker. Continue the current run instead of launching another DeLM runtime"
    );
    Ok(())
}

pub fn consume_capture(capture: &CapturedInvocation) -> Result<Binding> {
    let root = root(false)?.context("Missing native invocation")?;
    let registration: Registration = read_private(&session_path(&root, &capture.session_id))?;
    consume_launch_for(
        &capture.invocation_id,
        Some(&capture.session_id),
        registration.binding.owner,
    )
}

pub fn capture_run_id(capture: &CapturedInvocation) -> Result<Option<String>> {
    let root = root(false)?.context("Missing native invocation")?;
    let record: Registration = read_private(&session_path(&root, &capture.session_id))?;
    ensure!(
        record.binding.invocation_id == capture.invocation_id,
        "This capture belongs to an earlier invocation"
    );
    Ok(record.run_id)
}

/// Consume exactly one native launch, before any worker/model work. The runtime
/// is launched with `exec`, so its direct parent must still be the hook owner.
pub fn consume_launch(nonce: &str) -> Result<Binding> {
    consume_launch_for(
        nonce,
        std::env::var("CODEX_THREAD_ID").ok().as_deref(),
        ProcessIdentity::capture(unsafe { libc::getppid() } as u32)?,
    )
}

fn consume_launch_for(
    nonce: &str,
    thread: Option<&str>,
    owner: ProcessIdentity,
) -> Result<Binding> {
    validate_id(nonce)?;
    let thread = thread.context("Native thread identity is missing")?;
    validate_id(thread)?;
    let root = root(false)?.context("Native launch handshake is unavailable")?;
    let _lock = lock(&root, thread)?;
    let path = session_path(&root, thread);
    let mut registration: Registration = read_private(&path)?;
    let binding = registration.binding.clone();
    ensure!(binding.invocation_id == nonce, "Launch invocation mismatch");
    ensure!(
        thread == binding.session_id,
        "Native thread identity differs from its launch hook"
    );
    ensure!(
        owner == binding.owner,
        "Native host identity differs from its launch hook"
    );
    ensure!(
        !registration.consumed,
        "Native launch was already consumed; reconnect to the existing DeLM invocation"
    );
    ensure!(
        registration.pending.is_none() && !registration.finished,
        "Native launch was cancelled before admission"
    );
    binding.check_resources()?;
    ensure!(
        digest(&binding.executable)? == binding.executable_hash
            && digest(&binding.hooks_file)? == binding.hooks_hash,
        "Native lifecycle resources changed before admission"
    );
    registration.consumed = true;
    registration.runtime = Some(ProcessIdentity::capture(std::process::id())?);
    write_private(&path, &registration)?;
    Ok(binding)
}

impl Binding {
    /// Runtime calls this independently of model status polling. A disappeared
    /// host or changed/removed hook resource fails closed and preserves results.
    pub fn check_resources(&self) -> Result<()> {
        ensure!(
            self.owner.is_running()?,
            "Native owner exited or changed identity"
        );
        ensure!(
            FileIdentity::capture(&self.hooks_file)? == self.hooks_identity,
            "Native lifecycle hooks changed"
        );
        ensure!(
            FileIdentity::capture(&self.executable)? == self.executable_identity,
            "Native lifecycle executable changed"
        );
        Ok(())
    }

    pub fn register(&self, run_id: &str) -> Result<()> {
        self.check_resources()?;
        validate_id(run_id)?;
        validate_id(&self.session_id)?;
        validate_id(&self.invocation_id)?;
        let root = root(false)?.context("Missing lifecycle storage")?;
        let _lock = lock(&root, &self.session_id)?;
        let path = session_path(&root, &self.session_id);
        let mut registration: Registration = read_private(&path)?;
        ensure!(
            registration.binding.invocation_id == self.invocation_id && registration.consumed,
            "Native launch registration differs from this invocation"
        );
        ensure!(
            registration.runtime == Some(ProcessIdentity::capture(std::process::id())?),
            "Only the admitted runtime may register a run"
        );
        ensure!(
            registration.pending.is_none() && !registration.finished,
            "Native invocation was cancelled during preflight"
        );
        ensure!(
            registration
                .run_id
                .as_deref()
                .is_none_or(|old| old == run_id),
            "Native invocation already owns another run"
        );
        registration.run_id = Some(run_id.into());
        write_private(&path, &registration)
    }

    pub fn accepts(&self, signal: &Signal) -> Result<bool> {
        if !matches!(signal.event.as_str(), "Interrupt" | "Stop") {
            return Ok(false);
        }
        if self.session_id != signal.session_id
            || self.invocation_id != signal.invocation_id
            || self.owner != signal.owner
        {
            return Ok(false);
        }
        let Some(root) = root(false)? else {
            return Ok(false);
        };
        let path = session_path(&root, &self.session_id);
        if !path.try_exists()? {
            return Ok(false);
        }
        let registration: Registration = read_private(&path)?;
        Ok(registration.binding.invocation_id == self.invocation_id
            && !registration.finished
            && registration.pending.as_ref() == Some(signal)
            && (signal.event == "Interrupt"
                || signal.turn_id.as_deref() == Some(&registration.binding.turn_id)))
    }

    /// Durable signals cover cancellation during compatibility checks and a
    /// missed/failed best-effort control connection. Reading never clears one.
    pub fn pending_signal(&self) -> Result<Option<Signal>> {
        let root = root(false)?.context("Missing native lifecycle ownership")?;
        let registration: Registration = read_private(&session_path(&root, &self.session_id))?;
        ensure!(
            registration.binding.invocation_id == self.invocation_id,
            "Native invocation was replaced"
        );
        Ok(registration.pending)
    }

    pub fn ensure_admissible(&self) -> Result<()> {
        self.check_resources()?;
        ensure!(
            self.pending_signal()?.is_none(),
            "Native invocation was cancelled before worker admission"
        );
        Ok(())
    }

    /// Only acknowledge the exact signal handled; a newer Interrupt must survive
    /// acknowledgement of an earlier Stop while awaiting user input.
    pub fn acknowledge_signal(&self, signal: &Signal) -> Result<()> {
        let root = root(false)?.context("Missing lifecycle storage")?;
        let _lock = lock(&root, &self.session_id)?;
        let path = session_path(&root, &self.session_id);
        let mut registration: Registration = read_private(&path)?;
        ensure!(
            registration.binding.invocation_id == self.invocation_id,
            "Native invocation was replaced"
        );
        if registration.pending.as_ref() == Some(signal) {
            registration.pending = None;
            write_private(&path, &registration)?;
        }
        Ok(())
    }

    pub fn unregister(&self) -> Result<()> {
        let Some(root) = root(false)? else {
            return Ok(());
        };
        let _lock = lock(&root, &self.session_id)?;
        let path = session_path(&root, &self.session_id);
        if path.try_exists()? {
            let mut current: Registration = read_private(&path)?;
            if current.binding.invocation_id == self.invocation_id {
                current.finished = true;
                current.pending = None;
                write_private(&path, &current)?;
            }
        }
        Ok(())
    }

    /// Retire a consumed invocation that failed before a run was registered.
    /// A delayed startup finalizer must never finish a replacement invocation
    /// or take over the existing run's shutdown path.
    pub(crate) fn finish_startup(&self) -> Result<()> {
        let Some(root) = root(false)? else {
            return Ok(());
        };
        let _lock = lock(&root, &self.session_id)?;
        let path = session_path(&root, &self.session_id);
        if !path.try_exists()? {
            return Ok(());
        }
        let mut current: Registration = read_private(&path)?;
        if current.binding.invocation_id != self.invocation_id
            || current.finished
            || current.run_id.is_some()
        {
            return Ok(());
        }
        ensure!(
            current.binding.session_id == self.session_id
                && current.binding.owner == self.owner
                && current.consumed
                && current.runtime == Some(ProcessIdentity::capture(std::process::id())?),
            "Only the owning startup process may finish this native invocation"
        );
        current.finished = true;
        current.pending = None;
        write_private(&path, &current)
    }
}

/// This validates native discovery, not an active session's cached hook engine.
/// It cannot prove per-thread plugin selection or promise cancellation after a
/// user disables/untrusts the plugin. Stop active work before changing hooks.
pub fn validate_hook_listing(listing: &Value, executable: &Path) -> Result<()> {
    let source = executable
        .parent()
        .and_then(Path::parent)
        .context("Invalid installed executable")?
        .join("hooks/hooks.json");
    let entries = listing["data"]
        .as_array()
        .context("Native hooks/list omitted data")?;
    let hooks = entries
        .iter()
        .flat_map(|entry| entry["hooks"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    for event in REQUIRED_EVENTS {
        ensure!(
            hooks.iter().any(|hook| hook["eventName"] == event
                && hook["source"] == "plugin"
                && hook["sourcePath"] == source.to_string_lossy().as_ref()
                && hook["handlerType"] == "command"
                && hook["enabled"] == true
                && hook["async"] == false
                && hook["matcher"].is_null()
                && hook["timeoutSec"] == if event == "interrupt" { 3 } else { 5 }
                && matches!(hook["trustStatus"].as_str(), Some("trusted" | "managed"))
                && hook["command"] == format!("exec \"{}\" lifecycle-hook", executable.display())),
            "Required DeLM {event} hook is missing, disabled, changed, or untrusted; review /hooks in Codex"
        );
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn input(event: &str, session: &str, turn: &str) -> HookInput {
        serde_json::from_value(json!({"hook_event_name":event,"session_id":session,"turn_id":turn}))
            .unwrap()
    }

    pub(crate) fn make_binding() -> Binding {
        let executable = std::env::current_exe().unwrap();
        let identity = FileIdentity::capture(&executable).unwrap();
        Binding {
            session_id: uuid::Uuid::new_v4().to_string(),
            invocation_id: uuid::Uuid::new_v4().to_string(),
            turn_id: uuid::Uuid::new_v4().to_string(),
            owner: ProcessIdentity::capture(unsafe { libc::getppid() } as u32).unwrap(),
            executable: executable.clone(),
            executable_hash: String::new(),
            executable_identity: identity.clone(),
            hooks_identity: identity,
            hooks_file: executable,
            hooks_hash: String::new(),
        }
    }

    pub(crate) fn seed(binding: &Binding, consumed: bool) {
        let root = root(true).unwrap().unwrap();
        write_private(
            &session_path(&root, &binding.session_id),
            &Registration {
                binding: binding.clone(),
                run_id: None,
                runtime: consumed.then(|| ProcessIdentity::capture(std::process::id()).unwrap()),
                consumed,
                finished: false,
                pending: None,
                previous_invocations: Vec::new(),
                previous_turns: Vec::new(),
            },
        )
        .unwrap();
    }

    pub(crate) fn cleanup(binding: &Binding) {
        if let Some(root) = root(false).unwrap() {
            let _ = fs::remove_file(session_path(&root, &binding.session_id));
        }
    }

    pub(crate) struct CaptureFixture {
        _temp: tempfile::TempDir,
        pub executable: PathBuf,
        pub project: PathBuf,
        pub session: String,
        pub turn: String,
    }

    impl CaptureFixture {
        pub fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let base = temp.path().canonicalize().unwrap();
            let executable = base.join("bin/delm");
            let project = base.join("project");
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::create_dir_all(base.join("hooks")).unwrap();
            fs::create_dir_all(&project).unwrap();
            fs::write(&executable, "native capture fixture").unwrap();
            fs::write(base.join("hooks/hooks.json"), "{}").unwrap();
            Self {
                _temp: temp,
                executable,
                project,
                session: uuid::Uuid::new_v4().to_string(),
                turn: uuid::Uuid::new_v4().to_string(),
            }
        }
        pub fn submit(&self) -> HookAction {
            let mut event = input("UserPromptSubmit", &self.session, &self.turn);
            event.cwd = Some(self.project.clone());
            event.prompt = Some("$delm:run Do a small task".into());
            prepare_hook(event, &self.executable).unwrap()
        }
    }
    impl Drop for CaptureFixture {
        fn drop(&mut self) {
            if let Some(root) = root(false).unwrap() {
                // Remove only the UUID-namespaced records created by this test.
                for entry in fs::read_dir(root).unwrap() {
                    let path = entry.unwrap().path();
                    if path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .contains(&self.session)
                    {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
    }

    #[test]
    fn duplicate_native_invocation_has_one_launch_and_stable_capture() {
        let fixture = CaptureFixture::new();
        let results: Vec<_> = std::thread::scope(|scope| {
            let pending: Vec<_> = (0..8).map(|_| scope.spawn(|| fixture.submit())).collect();
            pending
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect()
        });
        assert_eq!(
            results
                .iter()
                .filter(|result| result.launch.is_some())
                .count(),
            1
        );
        assert!(
            results
                .iter()
                .all(|result| result.output == results[0].output)
        );
        let capture = results
            .into_iter()
            .find_map(|result| result.launch)
            .unwrap();
        assert!(capture.captured_at_ms > 0);
        assert_eq!(
            read_capture(&capture.path).unwrap().captured_at_ms,
            capture.captured_at_ms
        );
        assert!(matches!(
            capture_launch(&capture).unwrap(),
            Some(CaptureLaunch::Pending)
        ));
        assert!(capture_run_id(&capture).unwrap().is_none());
    }

    #[test]
    fn startup_finish_releases_failed_invocation_for_chat_and_fresh_launch() {
        let mut fixture = CaptureFixture::new();
        let capture = fixture.submit().launch.unwrap();
        let binding = consume_capture(&capture).unwrap();
        prepare_hook(
            input("Stop", &fixture.session, &fixture.turn),
            &fixture.executable,
        )
        .unwrap();
        assert!(binding.pending_signal().unwrap().is_some());
        binding.finish_startup().unwrap();

        let root = root(false).unwrap().unwrap();
        let record: Registration = read_private(&session_path(&root, &fixture.session)).unwrap();
        assert!(record.finished);
        assert!(record.pending.is_none() && record.run_id.is_none());
        let ordinary_turn = uuid::Uuid::new_v4().to_string();
        let ordinary = prepare_hook(
            input("UserPromptSubmit", &fixture.session, &ordinary_turn),
            &fixture.executable,
        )
        .unwrap();
        assert!(
            ordinary.output.is_none() && ordinary.launch.is_none() && ordinary.delivery.is_none()
        );

        // The failed runtime is still this live test process. Explicit startup
        // completion, rather than waiting for its death, permits the new turn.
        fixture.turn = uuid::Uuid::new_v4().to_string();
        let fresh = fixture.submit().launch.unwrap();
        assert_ne!(fresh.invocation_id, capture.invocation_id);
        assert!(consume_capture(&fresh).is_ok());
    }

    #[test]
    fn startup_finish_cannot_retire_a_concurrent_replacement() {
        let mut fixture = CaptureFixture::new();
        let capture = fixture.submit().launch.unwrap();
        let binding = consume_capture(&capture).unwrap();
        binding.finish_startup().unwrap();
        fixture.turn = uuid::Uuid::new_v4().to_string();
        let fresh = std::thread::scope(|scope| {
            let stale = scope.spawn(|| {
                for _ in 0..8 {
                    binding.finish_startup().unwrap();
                }
            });
            let fresh = fixture.submit().launch.unwrap();
            consume_capture(&fresh).unwrap();
            stale.join().unwrap();
            fresh
        });
        binding.finish_startup().unwrap();
        let root = root(false).unwrap().unwrap();
        let record: Registration = read_private(&session_path(&root, &fixture.session)).unwrap();
        assert_eq!(record.binding.invocation_id, fresh.invocation_id);
        assert!(record.consumed && !record.finished);
    }

    #[test]
    fn startup_finish_requires_its_runtime_and_preserves_registered_run() {
        let fixture = CaptureFixture::new();
        let capture = fixture.submit().launch.unwrap();
        let binding = consume_capture(&capture).unwrap();
        let root = root(false).unwrap().unwrap();
        let path = session_path(&root, &fixture.session);
        let mut record: Registration = read_private(&path).unwrap();
        let own_runtime = record.runtime;
        record.runtime = Some(binding.owner);
        write_private(&path, &record).unwrap();
        assert!(binding.finish_startup().is_err());
        assert!(!read_private::<Registration>(&path).unwrap().finished);

        record.runtime = own_runtime;
        write_private(&path, &record).unwrap();
        let run_id = uuid::Uuid::new_v4().to_string();
        binding.register(&run_id).unwrap();
        binding.finish_startup().unwrap();
        let record: Registration = read_private(&path).unwrap();
        assert_eq!(record.run_id.as_deref(), Some(run_id.as_str()));
        assert!(!record.finished);
    }

    #[test]
    fn canonical_launch_quotes_shell_metacharacters() {
        assert_eq!(
            launch_prefix(Path::new("/tmp/a'b/$() delm")),
            "exec '/tmp/a'\\''b/$() delm' run "
        );
    }

    #[test]
    fn unrelated_chats_do_not_create_session_records() {
        let binding = make_binding();
        for event in [
            "Interrupt",
            "Stop",
            "SessionEnd",
            "UserPromptSubmit",
            "PostToolUse",
        ] {
            let action = prepare_hook(
                input(event, &binding.session_id, &binding.turn_id),
                Path::new("/not/a/plugin"),
            )
            .unwrap();
            assert!(action.output.is_none() && action.delivery.is_none());
        }
        let mut unrelated = input("PreToolUse", &binding.session_id, &binding.turn_id);
        unrelated.tool_name = Some("exec_command".into());
        unrelated.tool_input = json!({"cmd":"echo normal chat"});
        let action = prepare_hook(unrelated, &std::env::current_exe().unwrap()).unwrap();
        assert!(action.output.is_none() && action.delivery.is_none());
        if let Some(root) = root(false).unwrap() {
            assert!(
                !root
                    .join(format!("session-{}.json", binding.session_id))
                    .exists()
            );
        }
        for event in [
            "Interrupt",
            "Stop",
            "SessionEnd",
            "UserPromptSubmit",
            "FutureEvent",
        ] {
            let action = prepare_hook(
                input(event, "future-non-uuid-session", "future-turn"),
                Path::new("/not/a/plugin"),
            )
            .unwrap();
            assert!(action.output.is_none() && action.delivery.is_none());
        }
    }

    #[test]
    fn scoped_turn_advance_rejects_stale_events_and_other_invocations() {
        let binding = make_binding();
        let run = uuid::Uuid::new_v4().to_string();
        seed(&binding, true);
        binding.register(&run).unwrap();
        let result = || {
            let action = prepare_hook(
                input("Interrupt", &binding.session_id, &binding.turn_id),
                Path::new("/ignored"),
            )
            .unwrap();
            let mut signal = action.delivery.unwrap().signal;
            assert!(binding.accepts(&signal).unwrap());
            binding.acknowledge_signal(&signal).unwrap();
            signal.invocation_id = uuid::Uuid::new_v4().to_string();
            assert!(!binding.accepts(&signal).unwrap());
            let new_turn = uuid::Uuid::new_v4().to_string();
            prepare_hook(
                input("UserPromptSubmit", &binding.session_id, &new_turn),
                Path::new("/ignored"),
            )
            .unwrap();
            prepare_hook(
                input("UserPromptSubmit", &binding.session_id, &binding.turn_id),
                Path::new("/ignored"),
            )
            .unwrap();
            assert!(
                prepare_hook(
                    input("Interrupt", &binding.session_id, &binding.turn_id),
                    Path::new("/ignored")
                )
                .unwrap()
                .delivery
                .is_none()
            );
            let action = prepare_hook(
                input("Stop", &binding.session_id, &new_turn),
                Path::new("/ignored"),
            )
            .unwrap();
            assert!(binding.accepts(&action.delivery.unwrap().signal).unwrap());
            let other = make_binding();
            assert!(
                prepare_hook(
                    input("Interrupt", &other.session_id, &new_turn),
                    Path::new("/ignored")
                )
                .unwrap()
                .delivery
                .is_none()
            );
        };
        let outcome = std::panic::catch_unwind(result);
        binding.unregister().unwrap();
        cleanup(&binding);
        outcome.unwrap();
    }

    #[test]
    fn native_listing_requires_every_trusted_exact_plugin_handler() {
        let executable = Path::new("/tmp/plugin/bin/delm");
        let mut hooks = REQUIRED_EVENTS
            .iter()
            .map(|event| {
                json!({"eventName":event,"source":"plugin",
            "sourcePath":"/tmp/plugin/hooks/hooks.json","handlerType":"command","enabled":true,
            "async":false,"matcher":null,"timeoutSec":if *event == "interrupt" {3}else{5},
            "trustStatus":"trusted","command":"exec \"/tmp/plugin/bin/delm\" lifecycle-hook"})
            })
            .collect::<Vec<_>>();
        assert!(validate_hook_listing(&json!({"data":[{"hooks":hooks}]}), executable).is_ok());
        for index in 0..hooks.len() {
            hooks[index]["trustStatus"] = json!("untrusted");
            assert!(validate_hook_listing(&json!({"data":[{"hooks":hooks}]}), executable).is_err());
            hooks[index]["trustStatus"] = json!("trusted");
        }
        hooks[0]["command"] = json!("echo lifecycle-hook");
        assert!(validate_hook_listing(&json!({"data":[{"hooks":hooks}]}), executable).is_err());
    }

    #[test]
    fn native_launch_is_one_use_bound_and_does_not_change_permission_policy() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("bin")).unwrap();
        fs::create_dir(directory.path().join("hooks")).unwrap();
        fs::create_dir(directory.path().join("project")).unwrap();
        let executable = directory.path().join("bin/delm");
        fs::write(&executable, b"fixture executable").unwrap();
        fs::write(directory.path().join("hooks/hooks.json"), b"{}").unwrap();
        let executable = executable.canonicalize().unwrap();
        let binding = make_binding();
        let nonce = uuid::Uuid::new_v4().to_string();
        let mut input = input("PreToolUse", &binding.session_id, &binding.turn_id);
        input.tool_name = Some("Bash".into());
        input.tool_input = json!({"command":format!("{}--launch-token {nonce} --project '{}'",launch_prefix(&executable),directory.path().join("project").display())});
        let output = prepare_hook(input, &executable).unwrap();
        assert!(output.output.is_none() && output.delivery.is_none());
        assert!(consume_launch_for(&nonce, Some("wrong-session"), binding.owner).is_err());
        let mut wrong_owner = binding.owner;
        wrong_owner.started_micros += 1;
        assert!(consume_launch_for(&nonce, Some(&binding.session_id), wrong_owner).is_err());
        let launch = consume_launch_for(&nonce, Some(&binding.session_id), binding.owner).unwrap();
        assert_eq!(launch.invocation_id, nonce);
        assert!(consume_launch_for(&nonce, Some(&binding.session_id), binding.owner).is_err());
        fs::write(&executable, b"changed fixture").unwrap();
        assert!(launch.check_resources().is_err());
        cleanup(&launch);
    }

    #[test]
    fn cancellation_before_registration_is_durable_and_interrupt_cannot_be_downgraded() {
        let binding = make_binding();
        seed(&binding, true);
        let action = prepare_hook(
            input("Interrupt", &binding.session_id, &binding.turn_id),
            Path::new("/ignored"),
        )
        .unwrap();
        assert!(action.delivery.is_none());
        let interrupt = binding.pending_signal().unwrap().unwrap();
        assert!(binding.register(&uuid::Uuid::new_v4().to_string()).is_err());
        prepare_hook(
            input("Stop", &binding.session_id, &binding.turn_id),
            Path::new("/ignored"),
        )
        .unwrap();
        assert_eq!(binding.pending_signal().unwrap(), Some(interrupt.clone()));
        let later = uuid::Uuid::new_v4().to_string();
        prepare_hook(
            input("UserPromptSubmit", &binding.session_id, &later),
            Path::new("/ignored"),
        )
        .unwrap();
        assert!(binding.accepts(&interrupt).unwrap());
        let mut earlier_stop = interrupt.clone();
        earlier_stop.event = "Stop".into();
        binding.acknowledge_signal(&earlier_stop).unwrap();
        assert_eq!(binding.pending_signal().unwrap(), Some(interrupt));
        assert!(
            prepare_hook(
                input("SessionEnd", &binding.session_id, &later),
                Path::new("/ignored")
            )
            .unwrap()
            .delivery
            .is_none()
        );
        cleanup(&binding);
    }

    #[test]
    fn launch_paths_are_literal_and_control_storage_cannot_be_a_project() {
        assert!(project_from_launch("--launch-token abc --project '/tmp'").is_err());
        if Path::new("/private/TMP").exists() {
            assert!(project_from_launch("--launch-token abc --project '/private/TMP'").is_err());
        }
        for command in [
            "--project $HOME",
            "--project /tmp;touch /tmp/bad",
            "--project \"$(pwd)\"",
            "--project /tmp && true",
        ] {
            assert!(literal_words(command).is_err());
        }
        assert_eq!(
            literal_words("--project '/tmp/a; $b'").unwrap(),
            ["--project", "/tmp/a; $b"]
        );
    }

    #[test]
    fn prior_question_stop_cannot_cancel_the_answered_turn() {
        let binding = make_binding();
        seed(&binding, true);
        binding.register(&uuid::Uuid::new_v4().to_string()).unwrap();
        let old = prepare_hook(
            input("Stop", &binding.session_id, &binding.turn_id),
            Path::new("/ignored"),
        )
        .unwrap()
        .delivery
        .unwrap()
        .signal;
        let next = uuid::Uuid::new_v4().to_string();
        prepare_hook(
            input("UserPromptSubmit", &binding.session_id, &next),
            Path::new("/ignored"),
        )
        .unwrap();
        assert!(!binding.accepts(&old).unwrap());
        assert!(binding.pending_signal().unwrap().is_none());
        cleanup(&binding);
    }

    #[test]
    fn different_native_sessions_do_not_share_a_cancel_lock() {
        let first = make_binding();
        let second = make_binding();
        seed(&first, true);
        seed(&second, true);
        let root = root(false).unwrap().unwrap();
        let _held = lock(&root, &first.session_id).unwrap();
        prepare_hook(
            input("Interrupt", &second.session_id, &second.turn_id),
            Path::new("/ignored"),
        )
        .unwrap();
        assert_eq!(second.pending_signal().unwrap().unwrap().event, "Interrupt");
        cleanup(&first);
        cleanup(&second);
    }

    #[test]
    fn replacing_an_unconsumed_reservation_invalidates_the_old_command() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["bin", "hooks", "project"] {
            fs::create_dir(directory.path().join(name)).unwrap();
        }
        let executable = directory.path().join("bin/delm");
        fs::write(&executable, b"fixture").unwrap();
        fs::write(directory.path().join("hooks/hooks.json"), b"{}").unwrap();
        let executable = executable.canonicalize().unwrap();
        let binding = make_binding();
        let make = |nonce: &str| {
            let mut value = input("PreToolUse", &binding.session_id, &binding.turn_id);
            value.tool_name = Some("Bash".into());
            value.tool_input = json!({"command":format!("{}--launch-token {nonce} --project '{}'",launch_prefix(&executable),directory.path().join("project").display())});
            value
        };
        let old = uuid::Uuid::new_v4().to_string();
        let new = uuid::Uuid::new_v4().to_string();
        prepare_hook(make(&old), &executable).unwrap();
        prepare_hook(make(&new), &executable).unwrap();
        assert!(consume_launch_for(&old, Some(&binding.session_id), binding.owner).is_err());
        let launch = consume_launch_for(&new, Some(&binding.session_id), binding.owner).unwrap();
        assert_eq!(launch.invocation_id, new);
        prepare_hook(
            input("Interrupt", &binding.session_id, &binding.turn_id),
            &executable,
        )
        .unwrap();
        assert!(launch.register(&uuid::Uuid::new_v4().to_string()).is_err());
        cleanup(&launch);
    }
}
