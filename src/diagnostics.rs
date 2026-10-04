//! Local inspection with an explicit export allowlist. Runtime evidence remains
//! private; reports contain counts, durations, and delivery state only.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const STATE_LIMIT: u64 = 64 * 1024 * 1024;
const JOURNAL_LIMIT: u64 = 512 * 1024 * 1024;
const LINE_LIMIT: usize = 16 * 1024 * 1024;
const ENTRY_LIMIT: usize = 1_000_000;
const PRUNABLE: &[&str] = &[
    "events.jsonl",
    "worker-1-capabilities.json",
    "worker-2-capabilities.json",
    "worker-1-thread.json",
    "worker-2-thread.json",
];

fn owned_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == unsafe { libc::getuid() },
        "Run evidence must be an owned regular file"
    );
    Ok(file)
}

fn read_json(path: &Path) -> Result<Option<Value>> {
    let file = match owned_file(path) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    ensure!(
        file.metadata()?.len() <= STATE_LIMIT,
        "Run state exceeds inspection limit"
    );
    let mut bytes = Vec::new();
    file.take(STATE_LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= STATE_LIMIT,
        "Run state exceeds inspection limit"
    );
    Ok(Some(
        serde_json::from_slice(&bytes).context("Run state is incomplete or invalid")?,
    ))
}

fn storage_root() -> Result<PathBuf> {
    // Read-only commands must not create storage on a new installation.
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?);
    Ok(home.join("Library/Application Support/DeLM/runs"))
}

fn owned_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::getuid() },
        "Run storage must be an owned real directory"
    );
    Ok(())
}

fn selected_run(id: &str) -> Result<PathBuf> {
    let id = uuid::Uuid::parse_str(id).context("Invalid run ID")?;
    let root = storage_root()?;
    owned_directory(root.parent().context("Missing storage root")?)?;
    owned_directory(&root)?;
    let path = root.join(id.to_string());
    owned_directory(&path).context("Run not found or storage is unavailable")?;
    Ok(path)
}

#[derive(Default)]
struct Storage {
    bytes: u64,
    files: usize,
}
fn measure(root: &Path) -> Result<Storage> {
    let mut total = Storage::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        total.files += 1;
        ensure!(
            total.files <= ENTRY_LIMIT,
            "Run has too many entries for bounded storage inspection"
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                ensure!(
                    pending.len() + total.files < ENTRY_LIMIT,
                    "Run has too many entries for bounded storage inspection"
                );
                pending.push(entry?.path());
            }
        } else {
            total.bytes = total.bytes.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

struct RunInfo {
    id: String,
    host: &'static str,
    status: String,
    state: Value,
    delivery: Value,
    bytes: u64,
    cleanup_blockers: Vec<&'static str>,
}

fn inspect(path: &Path) -> Result<RunInfo> {
    owned_directory(path)?;
    let claude = read_json(&path.join("claude.json"))?;
    let codex = read_json(&path.join("run.json"))?;
    ensure!(
        !(claude.is_some() && codex.is_some()),
        "Conflicting host records; preserve this run for recovery"
    );
    let (host, state) = if let Some(value) = claude {
        ("claude", value)
    } else if let Some(value) = codex {
        ("codex", value)
    } else {
        ("unknown", Value::Null)
    };
    let raw_status = state["status"].as_str().unwrap_or("unknown");
    // Never copy arbitrary strings from disk into a shareable report.
    let status = match raw_status {
        "prepared" | "preparing" | "starting" | "running" | "stopping" | "stopped" | "complete"
        | "delivered" | "delivery_conflict" | "recovery_required" | "error" | "failed"
        | "paused" | "startup_failed" | "preparation_failed" | "awaiting_shutdown"
        | "verifying_shutdown" => raw_status,
        _ => "unknown",
    }
    .to_owned();
    for relative in [
        "workspace",
        "workspace/delivery",
        "workspace/recovery",
        "board",
    ] {
        let directory = path.join(relative);
        if fs::symlink_metadata(&directory).is_ok() {
            owned_directory(&directory)?;
        }
    }
    let delivery = read_json(&path.join("workspace/delivery/result.json"))?.unwrap_or(Value::Null);
    let mut blockers = Vec::new();
    if !matches!(status.as_str(), "complete" | "delivered") {
        blockers.push("run_not_successfully_delivered");
    }
    if delivery["delivered"] != true {
        blockers.push("delivery_not_confirmed");
    }
    if delivery["cleanup_complete"] != true {
        blockers.push("workspace_cleanup_not_confirmed");
    }
    let workspace = path.join("workspace");
    if workspace.exists() {
        for entry in fs::read_dir(workspace)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let allowed =
                ["delivery", "recovery"].contains(&entry.file_name().to_string_lossy().as_ref());
            if kind.is_symlink() || (kind.is_dir() && !allowed) {
                blockers.push("temporary_workspaces_remain");
                break;
            }
        }
    }
    if host == "claude" {
        if state["finished"] != true {
            blockers.push("native_run_not_finished");
        }
    } else if host == "codex" {
        let shutdown = read_json(&path.join("shutdown-report.json"))?.unwrap_or(Value::Null);
        if shutdown["ownership_resolved"] != true
            || !shutdown["survivors"].as_array().is_some_and(Vec::is_empty)
            || !shutdown["errors"].as_array().is_some_and(Vec::is_empty)
        {
            blockers.push("native_shutdown_not_confirmed");
        }
    } else {
        blockers.push("native_ownership_unknown");
    }
    let watchdog = if host == "codex" {
        read_json(&path.join("watchdog.json"))?.unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let runtime = if host == "claude" {
        &state["runtime"]
    } else {
        &watchdog["runtime"]
    };
    match serde_json::from_value::<crate::supervisor::ProcessIdentity>(runtime.clone()) {
        Ok(identity) => match identity.is_running() {
            Ok(true) => blockers.push("runtime_still_running"),
            Ok(false) => {}
            Err(_) => blockers.push("runtime_ownership_unknown"),
        },
        Err(_) => blockers.push("runtime_ownership_unknown"),
    }
    Ok(RunInfo {
        id: path
            .file_name()
            .context("Missing run ID")?
            .to_string_lossy()
            .into_owned(),
        host,
        status,
        state,
        delivery,
        bytes: measure(path)?.bytes,
        cleanup_blockers: blockers,
    })
}

fn delivery_summary(value: &Value) -> Value {
    let count = |key: &str| value[key].as_array().map(Vec::len);
    json!({"delivered":value["delivered"].as_bool(),
        "cleanup_complete":value["cleanup_complete"].as_bool(),
        "verification_required":value["verification_required"].as_bool(),
        "changed_file_count":count("changed_paths"), "merged_file_count":count("merged_paths"),
        "conflict_count":count("conflicts"),
        "environment_manifest_count":count("environment_files_changed"),
        "omitted_environment_count":count("environment_directories_omitted"),
        "recovery_retained":value.get("recovery").map(Value::is_string)})
}

fn prune_candidates(path: &Path) -> Result<Vec<(String, u64)>> {
    let mut files = Vec::new();
    for name in PRUNABLE {
        match fs::symlink_metadata(path.join(name)) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file()
                        && metadata.nlink() == 1
                        && metadata.uid() == unsafe { libc::getuid() },
                    "Diagnostic cleanup target is not an owned regular file"
                );
                files.push(((*name).to_owned(), metadata.len()));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(files)
}

fn local_summary(path: &Path, info: &RunInfo) -> Value {
    let project = info.state["project"].as_str().or_else(|| {
        info.state
            .pointer("/workspace/original")
            .and_then(Value::as_str)
    });
    let prune = prune_candidates(path).ok();
    json!({"run_id":info.id,"host":info.host,"status":info.status,"project":project,
        "logical_bytes":info.bytes,"delivery":delivery_summary(&info.delivery),
        "diagnostic_cleanup_available":info.cleanup_blockers.is_empty() && prune.is_some(),
        "cleanup_blockers":info.cleanup_blockers,
        "reclaimable_logical_bytes":prune.map(|v| v.iter().map(|(_, size)| size).sum::<u64>())})
}

pub fn runs(as_json: bool) -> Result<()> {
    let root = storage_root()?;
    if !root.exists() {
        println!(
            "{}",
            if as_json {
                "[]"
            } else {
                "No DeLM runs are stored on this computer."
            }
        );
        return Ok(());
    }
    owned_directory(root.parent().context("Missing storage root")?)?;
    owned_directory(&root)?;
    let mut entries = Vec::new();
    for (index, entry) in fs::read_dir(&root)?.enumerate() {
        ensure!(index < 16384, "Too many runs to inspect safely");
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if uuid::Uuid::parse_str(&name).is_err() {
            continue;
        }
        let value = match inspect(&entry.path()) {
            Ok(info) => local_summary(&entry.path(), &info),
            Err(_) => json!({"run_id":name,"host":"unknown","status":"inspection_required",
                "diagnostic_cleanup_available":false,"cleanup_blockers":["state_unavailable_or_unsafe"]}),
        };
        entries.push(value);
    }
    entries.sort_by_key(|v| v["run_id"].as_str().unwrap_or_default().to_owned());
    if as_json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else if entries.is_empty() {
        println!("No DeLM runs are stored on this computer.");
    } else {
        println!(
            "RUN ID                                HOST     STATUS                 LOGICAL SIZE"
        );
        for entry in entries {
            println!(
                "{:<37} {:<8} {:<22} {}",
                entry["run_id"].as_str().unwrap_or("?"),
                entry["host"].as_str().unwrap_or("?"),
                entry["status"].as_str().unwrap_or("?"),
                entry["logical_bytes"]
                    .as_u64()
                    .map(|n| format!("{:.1} MiB", n as f64 / 1_048_576.))
                    .unwrap_or_else(|| "unavailable".into())
            );
            if let Some(project) = entry["project"].as_str() {
                let display = project
                    .chars()
                    .map(|c| {
                        if c.is_control() {
                            c.escape_default().to_string()
                        } else {
                            c.to_string()
                        }
                    })
                    .collect::<String>();
                println!("  {display}");
            }
        }
        println!(
            "\nInspect timing and delivery: delm report --run-id <ID>\nPreview diagnostic cleanup: delm clean --run-id <ID>"
        );
    }
    Ok(())
}

#[derive(Default)]
struct Timings {
    starts: BTreeMap<String, u64>,
    durations: BTreeMap<String, u64>,
    failed_phases: BTreeMap<String, u64>,
    phase_starts: BTreeMap<String, u64>,
    phase_ends: BTreeMap<String, u64>,
    first: Option<u64>,
    last: Option<u64>,
    first_action: Option<u64>,
    preparation_start: Option<u64>,
    clock_regressions: u64,
    invocation: Option<u64>,
    turns: HashMap<u64, u64>,
    intervals: Vec<(u64, u64, u64)>,
    waiting: HashMap<u64, u64>,
    waiting_ms: u64,
    waiting_seen: bool,
    records: u64,
    skipped: u64,
    partial: bool,
    response_count: u64,
    response_bytes: u64,
    response_max: u64,
    legacy_response_count: u64,
    legacy_response_bytes: u64,
    legacy_response_max: u64,
}

impl Timings {
    fn record(&mut self, entry: &Value) {
        let Some(time) = entry["time_ms"]
            .as_u64()
            .or_else(|| entry["time"].as_u64().map(|s| s.saturating_mul(1000)))
        else {
            self.skipped += 1;
            return;
        };
        self.records += 1;
        if self.last.is_some_and(|previous| time < previous) {
            self.clock_regressions += 1;
        }
        self.first = Some(self.first.map_or(time, |n| n.min(time)));
        self.last = Some(self.last.map_or(time, |n| n.max(time)));
        let data = &entry["data"];
        let kind = entry["kind"].as_str().unwrap_or("");
        let worker = data["worker"].as_u64();
        match kind {
            "phase" => {
                if let Some(phase) = data["phase"].as_str().filter(|p| {
                    matches!(
                        *p,
                        "preparation"
                            | "worker_admission"
                            | "shutdown"
                            | "delivery"
                            | "cleanup"
                            | "delivery_and_cleanup"
                            | "recovery_and_cleanup"
                            | "handoff"
                    )
                }) {
                    if data["boundary"] == "end" && data["success"] == false {
                        *self.failed_phases.entry(phase.into()).or_default() += 1;
                    }
                    if matches!(data["boundary"].as_str(), Some("start" | "end")) {
                        self.phase(phase, data["boundary"] == "start", time);
                    }
                }
            }
            "preparation_started" => {
                self.invocation = data["invocation_received_at_ms"].as_u64();
                self.phase("preparation", true, time);
            }
            "workspaces_prepared" => self.phase("preparation", false, time),
            "shutdown_started" => self.phase("shutdown", true, time),
            "shutdown_finished" => self.phase("shutdown", false, time),
            "worker_first_action" => {
                self.first_action = Some(self.first_action.map_or(time, |n| n.min(time)))
            }
            "worker_turn_started" => {
                if let Some(worker) = worker {
                    self.turns.entry(worker).or_insert(time);
                    if let Some(start) = self.waiting.remove(&worker) {
                        self.waiting_ms += time.saturating_sub(start);
                    }
                }
            }
            "worker_turn_finished" => {
                if let Some(worker) = worker {
                    if let Some(start) = self.turns.remove(&worker) {
                        self.intervals.push((worker, start, time));
                    }
                    if let Some(waiting) = data["waiting"].as_bool() {
                        self.waiting_seen = true;
                        if waiting {
                            self.waiting.insert(worker, time);
                        }
                    }
                }
            }
            "claude_coordination" => {
                if let Some(response) = data.get("response")
                    && let Ok(bytes) = serde_json::to_vec(response)
                {
                    self.legacy_response_count += 1;
                    self.legacy_response_bytes += bytes.len() as u64;
                    self.legacy_response_max = self.legacy_response_max.max(bytes.len() as u64);
                }
            }
            "coordination_response" => {
                if let Some(bytes) = data["bytes"].as_u64() {
                    self.response(bytes);
                }
            }
            _ => {}
        }
    }
    fn response(&mut self, bytes: u64) {
        self.response_count += 1;
        self.response_bytes += bytes;
        self.response_max = self.response_max.max(bytes);
    }
    fn phase(&mut self, name: &str, start: bool, time: u64) {
        if start {
            if name == "preparation" {
                self.preparation_start.get_or_insert(time);
            }
            self.phase_starts.entry(name.into()).or_insert(time);
            self.starts.entry(name.into()).or_insert(time);
        } else if let Some(start) = self.starts.remove(name) {
            self.phase_ends.insert(name.into(), time);
            *self.durations.entry(name.into()).or_default() += time.saturating_sub(start);
        }
    }
    fn summary(&self) -> Value {
        let phases: BTreeMap<_, _> = [
            "preparation",
            "worker_admission",
            "shutdown",
            "handoff",
            "delivery",
            "cleanup",
            "delivery_and_cleanup",
            "recovery_and_cleanup",
        ]
        .into_iter()
        .map(|name| (name, self.durations.get(name).copied()))
        .collect();
        let mut edges = Vec::new();
        let mut worker_ms = BTreeMap::new();
        for (worker, start, end) in &self.intervals {
            *worker_ms.entry(*worker).or_insert(0u64) += end.saturating_sub(*start);
            edges.push((*start, 1i32));
            edges.push((*end, -1i32));
        }
        edges.sort();
        let mut active = 0i32;
        let mut previous = 0;
        let mut overlap = 0;
        for (time, delta) in edges {
            if active >= 2 {
                overlap += time.saturating_sub(previous);
            }
            active += delta;
            previous = time;
        }
        let (response_count, response_bytes, response_max) = if self.response_count > 0 {
            (self.response_count, self.response_bytes, self.response_max)
        } else {
            (
                self.legacy_response_count,
                self.legacy_response_bytes,
                self.legacy_response_max,
            )
        };
        json!({"units":"milliseconds", "phases":phases,
            "failed_phase_counts":self.failed_phases,
            "open_phase_count":self.starts.len(),
            "preparation_to_admission_start_ms":self.phase_ends.get("preparation").zip(self.phase_starts.get("worker_admission")).map(|(a,b)| b.saturating_sub(*a)),
            "observed_journal_span_ms":self.first.zip(self.last).map(|(a,b)|b.saturating_sub(a)),
            "invocation_to_first_record_ms":self.invocation.zip(self.first).map(|(a,b)|b.saturating_sub(a)),
            "first_observed_action_after_preparation_start_ms":self.preparation_start.zip(self.first_action).map(|(a,b)|b.saturating_sub(a)),
            "closed_worker_turn_ms":worker_ms,"closed_worker_overlap_ms":(!self.intervals.is_empty()).then_some(overlap),
            "open_worker_turn_count":self.turns.len(),"closed_waiting_ms":self.waiting_seen.then_some(self.waiting_ms),
            "open_waiting_worker_count":self.waiting.len(),
            "records_read":self.records,"records_skipped":self.skipped,"partial_journal":self.partial,
            "out_of_order_timestamps":self.clock_regressions,
            "coordination_response_samples":response_count,
            "coordination_response_bytes":(response_count>0).then_some(response_bytes),
            "largest_coordination_response_bytes":(response_count>0).then_some(response_max),
            "response_samples_include_local_command_metadata":self.response_count>0,
            "interpretation":"Native turn intervals include model, tools, and host waits. They do not measure useful work or establish speedup. Open intervals are excluded. Null means evidence is unavailable; zero is a measured value."})
    }
}

fn timings(path: &Path) -> Result<Timings> {
    let mut result = Timings::default();
    let file = match owned_file(&path.join("events.jsonl")) {
        Ok(file) => file,
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(result);
        }
        Err(e) => return Err(e),
    };
    result.partial = file.metadata()?.len() > JOURNAL_LIMIT;
    let mut reader = BufReader::new(file.take(JOURNAL_LIMIT));
    let mut line = Vec::new();
    loop {
        line.clear();
        let bytes = Read::by_ref(&mut reader)
            .take(LINE_LIMIT as u64 + 1)
            .read_until(b'\n', &mut line)?;
        if bytes == 0 {
            break;
        }
        if line.len() > LINE_LIMIT {
            result.skipped += 1;
            result.partial = true;
            if line.last() != Some(&b'\n') {
                reader.skip_until(b'\n')?;
            }
            continue;
        }
        match serde_json::from_slice::<Value>(&line) {
            Ok(value) => result.record(&value),
            Err(_) => {
                result.skipped += 1;
                result.partial = true;
            }
        }
    }
    Ok(result)
}

fn check_counts(path: &Path) -> Result<Value> {
    let db_path = path.join("board/board.sqlite3");
    if !db_path.exists() {
        return Ok(json!({"receipt_count":null,"repeated_scope_and_command_count":null}));
    }
    let metadata = fs::symlink_metadata(&db_path)?;
    ensure!(
        metadata.is_file() && metadata.uid() == unsafe { libc::getuid() },
        "Unsafe board database"
    );
    let db = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(std::time::Duration::from_millis(250))?;
    let available: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='check_receipts'",
        [],
        |r| r.get(0),
    )?;
    if available == 0 {
        return Ok(
            json!({"receipt_count":null,"repeated_scope_and_command_count":null,"reason":"older_run_without_scoped_check_receipts"}),
        );
    }
    let mut statement = db.prepare("SELECT body FROM check_receipts LIMIT 100001")?;
    let mut rows = statement.query([])?;
    let mut seen = HashMap::<String, u64>::new();
    let mut count = 0u64;
    let mut repeated = 0;
    let mut total_bytes = 0usize;
    while let Some(row) = rows.next()? {
        ensure!(
            count < 100000,
            "Too many check receipts for bounded inspection"
        );
        let body: String = row.get(0)?;
        total_bytes = total_bytes.saturating_add(body.len());
        ensure!(
            total_bytes as u64 <= STATE_LIMIT,
            "Check receipts exceed the bounded inspection limit"
        );
        ensure!(
            body.len() <= LINE_LIMIT,
            "Check receipt exceeds inspection limit"
        );
        let value: Value = serde_json::from_str(&body)?;
        count += 1;
        let command = value
            .pointer("/evidence/command")
            .or_else(|| value.pointer("/native/command"));
        if let (Some(command), Some(files), Some(revision)) =
            (command, value.get("files"), value.get("request_revision"))
        {
            let signature = serde_json::to_string(&json!([command, files, revision]))?;
            let times = seen.entry(signature).or_default();
            if *times > 0 {
                repeated += 1;
            }
            *times += 1;
        }
    }
    Ok(
        json!({"receipt_count":count,"repeated_scope_and_command_count":repeated,
        "interpretation":"Same recorded command, explicit scoped file versions, and request revision. This does not prove equivalent environments, complete test coverage, or unnecessary work."}),
    )
}

// Reports remain an allowlist even when a retained summary is edited locally.
fn retained_timing(saved: &Value) -> Value {
    let mut safe = Timings::default().summary();
    for key in [
        "observed_journal_span_ms",
        "open_phase_count",
        "preparation_to_admission_start_ms",
        "invocation_to_first_record_ms",
        "first_observed_action_after_preparation_start_ms",
        "closed_worker_overlap_ms",
        "open_worker_turn_count",
        "closed_waiting_ms",
        "open_waiting_worker_count",
        "records_read",
        "records_skipped",
        "out_of_order_timestamps",
        "coordination_response_samples",
        "coordination_response_bytes",
        "largest_coordination_response_bytes",
    ] {
        safe[key] = saved[key].as_u64().map_or(Value::Null, |n| json!(n));
    }
    for key in [
        "partial_journal",
        "response_samples_include_local_command_metadata",
    ] {
        safe[key] = saved[key].as_bool().map_or(Value::Null, |n| json!(n));
    }
    if let Some(phases) = safe["phases"].as_object_mut() {
        for (name, value) in phases {
            *value = saved["phases"][name]
                .as_u64()
                .map_or(Value::Null, |n| json!(n));
        }
    }
    if let Some(phases) = safe["phases"].as_object() {
        let failures = phases
            .keys()
            .filter_map(|phase| {
                saved["failed_phase_counts"][phase]
                    .as_u64()
                    .map(|n| (phase.clone(), json!(n)))
            })
            .collect::<serde_json::Map<_, _>>();
        safe["failed_phase_counts"] = Value::Object(failures);
    }
    for worker in ["1", "2"] {
        if let Some(ms) = saved["closed_worker_turn_ms"][worker].as_u64() {
            safe["closed_worker_turn_ms"][worker] = json!(ms);
        }
    }
    safe["retained_after_diagnostic_cleanup"] = json!(true);
    safe
}

fn report_at(path: &Path) -> Result<Value> {
    let info = inspect(path)?;
    let timing = timings(path)?;
    let timing = if !path.join("events.jsonl").exists() {
        read_json(&path.join("diagnostics-summary.json"))?
            .map(|saved| retained_timing(&saved["timing"]))
            .unwrap_or_else(|| timing.summary())
    } else {
        timing.summary()
    };
    let checks = check_counts(path)?;
    Ok(
        json!({"schema_version":1,"report_runtime_version":env!("CARGO_PKG_VERSION"),
        "run_id":info.id,"host":info.host,"status":info.status,
        "storage":{"logical_bytes":info.bytes,"meaning":"Logical file sizes; APFS shared blocks and reclaimable physical space are not measured."},
        "delivery":delivery_summary(&info.delivery),"timing":timing,"scoped_checks":checks,
        "cleanup_blockers":info.cleanup_blockers,
        "privacy":"Allowlisted counts, durations, identifiers, and delivery flags only. No project paths, source, prompts, command text, native output, credentials, or recovery contents.",
        "coordination_measurement":"Recorded response bodies only. Older Claude records exclude appended local command metadata; absent response samples are unavailable, not zero."}),
    )
}

pub fn report(id: &str, output: Option<&Path>) -> Result<()> {
    let value = report_at(&selected_run(id)?)?;
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    if let Some(path) = output {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .context("Create a new diagnostics file; existing files are never overwritten")?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        println!("Saved redacted diagnostics to {}", path.display());
    } else {
        std::io::stdout().write_all(&bytes)?;
    }
    Ok(())
}

fn clean_at(path: &Path, confirm: bool) -> Result<Value> {
    let info = inspect(path)?;
    ensure!(
        info.cleanup_blockers.is_empty(),
        "Diagnostic cleanup is blocked: {}. Stop the run and resolve recovery first; no files were removed",
        info.cleanup_blockers.join(", ")
    );
    if !confirm {
        return Ok(clean_summary(&info, &prune_candidates(path)?, false));
    }
    // Reuse project admission locking so no recovery or resumed work can start
    // while its diagnostic evidence is being removed.
    let project = info.state["project"]
        .as_str()
        .or_else(|| {
            info.state
                .pointer("/workspace/original")
                .and_then(Value::as_str)
        })
        .context("Original project identity is missing; cleanup is blocked")?;
    ensure!(
        Path::new(project).is_absolute(),
        "Original project identity is invalid; cleanup is blocked"
    );
    let _lock = crate::run::state::RunLock::acquire_for_recovery(Path::new(project), &info.id)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path.join("diagnostic-cleanup.lock"))?;
    lock.try_lock_exclusive()
        .context("Diagnostic cleanup is already running")?;
    let info = inspect(path)?;
    ensure!(
        info.cleanup_blockers.is_empty(),
        "Run state changed; cleanup is blocked"
    );
    let files = prune_candidates(path)?;
    {
        // Preserve a safe report before removing the underlying verbose journal.
        let report = report_at(path)?;
        crate::run::state::atomic_json(&path.join("diagnostics-summary.json"), &report)?;
        for (name, _) in &files {
            let target = path.join(name);
            let file = owned_file(&target)?;
            let before = file.metadata()?;
            let now = fs::symlink_metadata(&target)?;
            ensure!(
                now.is_file()
                    && now.nlink() == 1
                    && before.dev() == now.dev()
                    && before.ino() == now.ino(),
                "Diagnostic file changed during cleanup"
            );
            fs::remove_file(target)?;
        }
    }
    Ok(clean_summary(&info, &files, true))
}

fn clean_summary(info: &RunInfo, files: &[(String, u64)], confirmed: bool) -> Value {
    json!({"run_id":info.id,"removed":confirmed,"files":files.iter().map(|(n,_)|n).collect::<Vec<_>>(),
        "logical_bytes":files.iter().fold(0u64, |sum, (_,bytes)|sum.saturating_add(*bytes)),
        "retained":"Run ownership, completion evidence, board, delivery journal, recovery material, executable, and original project are preserved.",
        "next":if confirmed {"Diagnostic cleanup completed."} else {"Preview only. Repeat with --confirm to remove these diagnostic files."}})
}

pub fn clean(id: &str, confirm: bool) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&clean_at(&selected_run(id)?, confirm)?)?
    );
    Ok(())
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
