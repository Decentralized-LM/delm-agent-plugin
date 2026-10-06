//! Passive display projection over saved state. The observer has no controller,
//! coordination lock, workspace handle, credential, or writable database.
use crate::{
    board::reader::{self, Page},
    run::state::now_ms,
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

const STATE_LIMIT: u64 = 64 * 1024 * 1024;
const JOURNAL_CHUNK: usize = 1024 * 1024;
const RECORD_LIMIT: usize = 8 * 1024 * 1024;
const PAGE_BUDGET: usize = 240 * 1024;
const OUTPUT_LIMIT: usize = 256 * 1024;

#[derive(Args)]
pub struct Arguments {
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    session_id: String,
    #[arg(long)]
    watch: bool,
    #[arg(long, default_value_t = 250, value_parser = clap::value_parser!(u64).range(250..=10_000))]
    interval_ms: u64,
    #[arg(long, value_parser = ["tasks", "shared", "checks"])]
    collection: Option<String>,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    #[arg(long, default_value_t = 8)]
    limit: usize,
    #[arg(long)]
    through_sequence: Option<u64>,
    #[arg(long, requires = "collection", conflicts_with_all = ["offset", "through_sequence"], value_parser = clap::value_parser!(u64).range(1..=i64::MAX as u64))]
    item_id: Option<u64>,
}

/// This intentionally does not use state::run_path: that helper creates missing
/// storage. Looking at a missing/old run must leave the filesystem untouched.
fn run_path(id: &str) -> Result<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?);
    run_path_in(&home, id)
}

/// Directory modes the observer refuses for run storage ancestors and for the
/// run itself.
const ANCESTOR_FORBIDDEN: u32 = 0o022;
const RUN_FORBIDDEN: u32 = 0o077;

fn run_path_in(home: &Path, id: &str) -> Result<PathBuf> {
    let id = uuid::Uuid::parse_str(id).context("Invalid view run identity")?;
    let parent = home.join("Library/Application Support").canonicalize()?;
    let root = parent.join("DeLM");
    let runs = root.join("runs");
    let run = runs.join(id.to_string());
    // The writer creates every run privately, but it accepts an existing owned
    // `runs` directory with any mode, and older releases created it with the
    // default 0755. The observer must accept every layout the writer runs in:
    // ancestors only have to be owned and unmodifiable by other users, which
    // already prevents another account from replacing a run. The run itself
    // stays private.
    owned_directory(&root, ANCESTOR_FORBIDDEN)?;
    owned_directory(&runs, ANCESTOR_FORBIDDEN)?;
    owned_directory(&run, RUN_FORBIDDEN)?;
    Ok(run)
}

fn owned_directory(path: &Path, forbidden: u32) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| {
            format!(
                "Board view cannot open {} as a real directory",
                path.display()
            )
        })?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
        "Board view storage {} must be a directory owned by the current user",
        path.display()
    );
    ensure!(
        meta.mode() & forbidden == 0,
        "Board view storage {} has permissions {:03o}; it must not be {} other users",
        path.display(),
        meta.mode() & 0o777,
        if forbidden == RUN_FORBIDDEN {
            "accessible to"
        } else {
            "writable by"
        }
    );
    Ok(())
}

fn open_file(path: &Path, limit: u64) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        // An unexpected FIFO must be rejected by metadata validation rather
        // than block before we can establish that this is a regular file.
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0
            && meta.len() <= limit,
        "Unsafe or oversized board view source"
    );
    Ok(file)
}

fn read_state(run: &Path, session: &str) -> Result<Value> {
    let mut file = open_file(&run.join("claude.json"), STATE_LIMIT)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(STATE_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= STATE_LIMIT,
        "Board view state exceeds its limit"
    );
    let saved: Value =
        serde_json::from_slice(&bytes).context("Saved board view state is incomplete")?;
    ensure!(
        saved["version"] == 1 && saved["host"] == "claude",
        "Unsupported saved board view version"
    );
    ensure!(
        saved["session_id"].as_str() == Some(session),
        "Board view does not belong to this conversation"
    );
    Ok(saved)
}

#[derive(Default)]
struct Journal {
    offset: u64,
    identity: Option<(u64, u64)>,
    pending: Vec<u8>,
    skipping: bool,
    complete: bool,
    final_event: Option<Value>,
    reason: Option<String>,
}

impl Journal {
    fn read(&mut self, run: &Path) -> Result<()> {
        let mut file = match open_file(&run.join("events.jsonl"), u64::MAX) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                self.complete = true;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let meta = file.metadata()?;
        let identity = (meta.dev(), meta.ino());
        ensure!(
            self.identity.is_none_or(|old| old == identity) && meta.len() >= self.offset,
            "Board view journal was replaced or truncated"
        );
        if self.identity.is_none() && meta.len() > RECORD_LIMIT as u64 {
            // Only terminal lifecycle records are needed. Start with a bounded
            // tail on restored runs, then consume complete appended records.
            self.offset = meta.len() - RECORD_LIMIT as u64;
            file.seek(SeekFrom::Start(self.offset - 1))?;
            let mut preceding = [0];
            file.read_exact(&mut preceding)?;
            self.skipping = preceding[0] != b'\n';
        }
        self.identity = Some(identity);
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        file.take(JOURNAL_CHUNK as u64).read_to_end(&mut bytes)?;
        self.offset += bytes.len() as u64;
        for byte in bytes {
            if byte == b'\n' {
                if !self.skipping && !self.pending.is_empty() {
                    // Most journal records contain private transport data. Only
                    // three lifecycle records are projected; no record is echoed.
                    if let Ok(record) = serde_json::from_slice::<Value>(&self.pending) {
                        match record["kind"].as_str() {
                            Some("claude_final") => {
                                self.final_event = Some(project_outcome(&record["data"]))
                            }
                            Some("claude_recovery_required" | "claude_cancelled") => {
                                self.reason = record["data"]["reason"]
                                    .as_str()
                                    .map(|s| reader::text(s, 2048));
                            }
                            _ => (),
                        }
                    }
                }
                self.pending.clear();
                self.skipping = false;
            } else if !self.skipping {
                if self.pending.len() == RECORD_LIMIT {
                    self.pending.clear();
                    self.skipping = true;
                } else {
                    self.pending.push(byte);
                }
            }
        }
        self.complete = self.offset >= meta.len() && self.pending.is_empty() && !self.skipping;
        Ok(())
    }
}

fn safe_string(value: &Value, max: usize) -> Value {
    value
        .as_str()
        .map(|s| {
            let mut text = reader::text(s, max);
            if text.len() > max {
                let mut end = max.saturating_sub('…'.len_utf8());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                text.push('…');
            }
            json!(text)
        })
        .unwrap_or(Value::Null)
}

fn project_outcome(event: &Value) -> Value {
    let delivery = &event["delivery"];
    let recovery = &event["recovery"];
    let recovery_path = delivery
        .get("recovery")
        .filter(|v| v.is_string())
        .unwrap_or(&recovery["recovery"]);
    let paths = |key: &str| {
        delivery[key]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .take(16)
                    .map(|value| safe_string(value, 512))
                    .filter(Value::is_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let path_count = |key: &str| delivery[key].as_array().map_or(0, Vec::len);
    let excluded = delivery["excluded_paths"].as_object().map(|values| values.iter().take(16)
        .map(|(path, reason)| json!({"path":reader::text(path,512),"reason":safe_string(reason,256)}))
        .collect::<Vec<_>>()).unwrap_or_default();
    json!({"status":safe_string(&event["status"],80),
        "delivered":delivery["delivered"].as_bool(),"verification_required":delivery["verification_required"].as_bool(),
        "cleanup_complete":delivery["cleanup_complete"].as_bool().or(recovery["cleanup_complete"].as_bool()),
        "recovery_saved":recovery_path.is_string().then_some(true),"recovery_path":safe_string(recovery_path,4096),
        "changed_paths_count":delivery["changed_paths"].as_array().map(Vec::len),
        "artifacts":paths("artifacts"),"artifacts_total":path_count("artifacts"),"artifacts_declared":delivery["artifacts_declared"].as_bool(),
        "undelivered_outputs":paths("undelivered_outputs"),"undelivered_outputs_total":path_count("undelivered_outputs"),
        "excluded_paths":excluded,"excluded_paths_total":delivery["excluded_paths"].as_object().map_or(0,serde_json::Map::len),
        "conflicts":delivery["conflicts"].as_array().map(|v|v.iter().take(16).map(|s|safe_string(s,512)).filter(Value::is_string).collect::<Vec<_>>()).unwrap_or_default(),
        "conflicts_total":delivery["conflicts"].as_array().map_or(0,Vec::len),
        "reason":safe_string(&event["reason"],2048),"retained_workspace_path":Value::Null})
}

fn agents(saved: &Value, board: &Value) -> Vec<Value> {
    saved["workers"].as_array().map(|workers|workers.iter().take(32).enumerate().map(|(index,worker)| {
        let slot=index+1;
        let reported=board["workers"].as_array().and_then(|items|items.iter().find(|item|item["slot"]==slot));
        let empty=Value::Null;
        let report=reported.unwrap_or(&empty);
        let native_state=if saved["finished"]==true {"stopped"}
            else if worker["blocked"]==true {"blocked"}
            else if worker["waiting"]==true {"waiting"}
            else if worker["turn_id"].is_string() {"working"}
            else if worker["agent_id"].is_string() {"ready"}
            else {"starting"};
        let task_ids=board["ownership"].as_array().map(|items|items.iter().filter(|item|item["slot"]==slot)
            .filter_map(|item|item["id"].as_u64()).take(64).collect::<Vec<_>>()).unwrap_or_default();
        // The initialized board rows are not explicit agent reports.
        let explicit=report["sequence"].as_u64().unwrap_or(0)>0;
        json!({"slot":slot,"name":format!("Agent {slot}"),"native_state":native_state,
            "native_agent_id":safe_string(&worker["agent_id"],256),"request_revision":worker["revision"].as_u64(),
            "reported_state":if explicit {report["reported_state"].clone()}else{Value::Null},
            "summary":if explicit {safe_string(&report["summary"],2048)}else{Value::Null},
            "dependency":if explicit {safe_string(&report["dependency"],512)}else{Value::Null},
            "task_count":board["ownership_counts"][slot.to_string()].as_u64().unwrap_or(task_ids.len() as u64),"task_ids":task_ids})
    }).collect()).unwrap_or_default()
}

fn empty_collection(page: Page) -> Value {
    json!({"items":[],"total":0,"offset":page.offset,"limit":page.limit,"next_offset":Value::Null,"through_sequence":Value::Null})
}

/// Keep complete detail rows and move the page boundary instead of truncating
/// an arbitrary JSON line. Totals remain authoritative, and the next request
/// starts immediately after the last returned row.
fn fit_pages(snapshot: &mut Value, selected: Option<&str>) -> Result<()> {
    const NAMES: [&str; 3] = ["tasks", "shared", "checks"];
    let mut size = serde_json::to_vec(snapshot)?.len();
    if size <= PAGE_BUDGET {
        return Ok(());
    }
    let mut row_sizes = NAMES
        .iter()
        .map(|name| {
            snapshot[*name]["items"]
                .as_array()
                .context("Missing view items")?
                .iter()
                .map(|item| {
                    serde_json::to_vec(item)
                        .map(|bytes| bytes.len() + 1)
                        .map_err(Into::into)
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let initial_counts = row_sizes.iter().map(Vec::len).collect::<Vec<_>>();
    while size > PAGE_BUDGET - 256 {
        let choose = |minimum: usize| {
            row_sizes
                .iter()
                .enumerate()
                .filter(|(index, rows)| {
                    rows.len()
                        > if selected == Some(NAMES[*index]) {
                            1
                        } else {
                            minimum
                        }
                })
                .max_by_key(|(_, rows)| rows.iter().sum::<usize>())
                .map(|(index, _)| index)
        };
        let index = choose(1)
            .or_else(|| choose(0))
            .context("Board view metadata exceeds its display budget")?;
        size = size.saturating_sub(row_sizes[index].pop().unwrap());
        snapshot[NAMES[index]]["items"]
            .as_array_mut()
            .unwrap()
            .pop();
    }
    for (index, name) in NAMES.iter().enumerate() {
        let page = &mut snapshot[*name];
        let count = page["items"].as_array().unwrap().len();
        if count < initial_counts[index] {
            page["requested_limit"] = page["limit"].clone();
            page["limit"] = json!(count);
        }
        let end = page["offset"].as_u64().unwrap_or(0) + count as u64;
        page["next_offset"] =
            if page["item_id"].is_null() && end < page["total"].as_u64().unwrap_or(0) {
                json!(end)
            } else {
                Value::Null
            };
    }
    ensure!(
        serde_json::to_vec(snapshot)?.len() <= PAGE_BUDGET,
        "Board view exceeds its display budget"
    );
    Ok(())
}

struct Observer {
    run: PathBuf,
    run_id: String,
    session: String,
    journal: Journal,
    last_board: Value,
    last_controller: u64,
    generation: u64,
    directory_identity: (u64, u64),
}

impl Observer {
    fn new(run: PathBuf, run_id: String, session: String) -> Result<Self> {
        owned_directory(&run, RUN_FORBIDDEN)?;
        ensure!(
            !session.is_empty() && session.len() <= 256,
            "Missing view conversation identity"
        );
        read_state(&run, &session)?;
        let run = run.canonicalize()?;
        let metadata = std::fs::symlink_metadata(&run)?;
        Ok(Self {
            run,
            run_id,
            session,
            journal: Journal::default(),
            last_board: Value::Null,
            last_controller: 0,
            generation: 0,
            directory_identity: (metadata.dev(), metadata.ino()),
        })
    }

    fn snapshot(&mut self, collection: Option<&str>, page: Page) -> Result<Value> {
        owned_directory(&self.run, RUN_FORBIDDEN)?;
        let metadata = std::fs::symlink_metadata(&self.run)?;
        ensure!(
            (metadata.dev(), metadata.ino()) == self.directory_identity
                && self.run.canonicalize()? == self.run,
            "Board view run directory was replaced"
        );
        let saved = read_state(&self.run, &self.session)?;
        let sequence = saved["sequence"].as_u64().unwrap_or(0);
        ensure!(
            sequence >= self.last_controller,
            "Board view lifecycle moved backwards"
        );
        self.last_controller = sequence;
        let mut unavailable = Vec::new();
        match reader::snapshot(&self.run, collection, page) {
            Ok(board) => {
                ensure!(
                    board["sequence"].as_u64().unwrap_or(0)
                        >= self.last_board["sequence"].as_u64().unwrap_or(0),
                    "Board view coordination moved backwards"
                );
                if board["revision"].as_u64() == Some(saved["revision"].as_u64().unwrap_or(1)) {
                    self.last_board = board;
                } else {
                    // SQLite and lifecycle metadata commit separately. Preserve
                    // the last compatible page until both sources acknowledge
                    // the new request; never label new claims with an old task.
                    unavailable.push("board_revision");
                }
            }
            Err(_) => unavailable.push("board"),
        }
        if self.journal.read(&self.run).is_err() {
            unavailable.push("journal");
        }
        // Finished runs have no future timer to fill in their final delivery.
        // Drain only the bounded tail, never rescan the full private journal.
        if saved["finished"] == true && !unavailable.contains(&"journal") {
            for _ in 0..(RECORD_LIMIT / JOURNAL_CHUNK + 1) {
                if self.journal.complete {
                    break;
                }
                if self.journal.read(&self.run).is_err() {
                    unavailable.push("journal");
                    break;
                }
            }
        }
        if !self.journal.complete && !unavailable.contains(&"journal") {
            unavailable.push("journal_catching_up");
        }
        let mut outcome = project_outcome(&saved);
        if saved["finished"] == true
            && let Some(final_event) = &self.journal.final_event
            && final_event["status"] == saved["status"]
        {
            outcome = final_event.clone();
        }
        if outcome["reason"].is_null() {
            outcome["reason"] = json!(self.journal.reason);
        }
        if saved["finished"] == true
            && matches!(
                saved["status"].as_str(),
                Some("complete" | "delivered" | "delivery_conflict")
            )
            && outcome["delivered"].is_null()
        {
            // A missing, malformed, or oversized terminal record is not proof
            // of delivery details, even when lifecycle completion was saved.
            unavailable.push("outcome");
        }
        if saved["status"] == "recovery_required" && outcome["cleanup_complete"] != true {
            outcome["retained_workspace_path"] = json!(reader::text(
                &self.run.join("workspace").to_string_lossy(),
                4096
            ));
        }
        if let Some(obj) = outcome.as_object_mut() {
            obj.remove("status");
        }
        self.generation += 1;
        let board = &self.last_board;
        let collection_value = |name: &str| {
            board
                .get(name)
                .cloned()
                .unwrap_or_else(|| empty_collection(page))
        };
        let mut snapshot = json!({"schema_version":1,"type":"view","run_id":self.run_id,"session_id":self.session,
            "revision":saved["revision"].as_u64().unwrap_or(1),"view_generation":self.generation,
            "source":{"controller_sequence":sequence,"board_sequence":board["sequence"].as_u64().unwrap_or(0),
                "board_revision":board["revision"].as_u64(),"journal_offset":self.journal.offset},
            "observed_at":now_ms(),"status":safe_string(&saved["status"],80),"finished":saved["finished"]==true,
            "freshness":{"unavailable":unavailable},"agents":agents(&saved,board),
            "tasks":collection_value("tasks"),"shared":collection_value("shared"),"checks":collection_value("checks"),"outcome":outcome});
        fit_pages(&mut snapshot, collection)?;
        Ok(snapshot)
    }
}

async fn emit(value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= OUTPUT_LIMIT,
        "Board view output exceeds its limit"
    );
    bytes.push(b'\n');
    let mut output = tokio::io::stdout();
    output.write_all(&bytes).await?;
    output.flush().await?;
    Ok(())
}

pub(super) async fn execute(args: Arguments) -> Result<()> {
    let page = Page {
        offset: args.offset,
        limit: args.limit,
        through_sequence: args.through_sequence,
        item_id: args.item_id,
    };
    ensure!(
        (1..=reader::MAX_PAGE).contains(&page.limit) && page.offset <= 1_000_000,
        "Invalid board view page"
    );
    let run = run_path(&args.run_id)?;
    let mut observer = Observer::new(run, args.run_id, args.session_id)?;
    let mut last = Value::Null;
    loop {
        let snapshot = observer.snapshot(args.collection.as_deref(), page)?;
        let mut stable = snapshot.clone();
        stable.as_object_mut().unwrap().remove("observed_at");
        stable.as_object_mut().unwrap().remove("view_generation");
        if stable != last {
            emit(&snapshot).await?;
            last = stable;
        }
        if !args.watch || snapshot["finished"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(args.interval_ms)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{board::Board, run::state::atomic_json};
    use std::{
        fs,
        io::Write,
        os::unix::fs::{PermissionsExt, symlink},
    };

    fn fixture() -> (tempfile::TempDir, Board, Value) {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["baseline", "worker1", "worker2"] {
            fs::create_dir(temp.path().join(name)).unwrap();
        }
        let board = Board::open(
            temp.path(),
            &temp.path().join("baseline"),
            [temp.path().join("worker1"), temp.path().join("worker2")],
        )
        .unwrap();
        let saved = json!({"version":1,"host":"claude","session_id":"session","sequence":3,"revision":1,
            "status":"running","finished":false,"token_digest":"DO NOT DISPLAY",
            "task":"PRIVATE REQUEST","workers":[{"agent_id":"a1","turn_id":"t1","revision":1},
            {"agent_id":"a2","turn_id":null,"revision":1}]});
        atomic_json(&temp.path().join("claude.json"), &saved).unwrap();
        (temp, board, saved)
    }

    fn observer(root: &Path) -> Observer {
        Observer::new(root.to_owned(), "run".into(), "session".into()).unwrap()
    }

    fn journal(root: &Path, records: &[Value]) {
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(root.join("events.jsonl"))
            .unwrap();
        for record in records {
            serde_json::to_writer(&mut file, record).unwrap();
            file.write_all(b"\n").unwrap();
        }
    }

    #[test]
    fn observer_is_read_only_and_does_not_invent_native_activity() {
        let (root, mut board, _) = fixture();
        let id=board.call(1,"delm_task_create",json!({"idempotency_key":"task","title":"Implement parser","description":"Parse selected files"})).unwrap()["result"]["task_id"].clone();
        board
            .call(
                1,
                "delm_task_claim",
                json!({"idempotency_key":"claim","task_id":id}),
            )
            .unwrap();
        journal(
            root.path(),
            &[json!({"kind":"claude_turn_completed","data":{"answer":"PRIVATE ANSWER"}})],
        );
        let metadata = fs::read(root.path().join("claude.json")).unwrap();
        let events = fs::read(root.path().join("events.jsonl")).unwrap();
        let before = board.view().unwrap();
        let mut view = observer(root.path());
        let snapshot = view.snapshot(None, Page::default()).unwrap();
        assert_eq!(snapshot["agents"][0]["native_state"], "working");
        assert_eq!(snapshot["agents"][1]["native_state"], "ready");
        assert_eq!(snapshot["agents"][0]["reported_state"], Value::Null);
        assert_eq!(snapshot["agents"][0]["task_ids"], json!([id]));
        assert_eq!(before, board.view().unwrap());
        assert_eq!(metadata, fs::read(root.path().join("claude.json")).unwrap());
        assert_eq!(events, fs::read(root.path().join("events.jsonl")).unwrap());
        assert!(!snapshot.to_string().contains("PRIVATE"));
        assert!(!snapshot.to_string().contains("DO NOT DISPLAY"));
    }

    #[test]
    fn final_delivery_waits_for_matching_finished_lifecycle_and_retains_verification() {
        let (root, _board, mut saved) = fixture();
        journal(
            root.path(),
            &[json!({"kind":"claude_final","data":{"status":"delivered",
            "delivery":{"delivered":true,"verification_required":true,"cleanup_complete":true,"changed_paths":["main.rs"],"conflicts":[]},
            "checks":[{"command":"PRIVATE COMMAND"}]}})],
        );
        let mut view = observer(root.path());
        assert_eq!(
            view.snapshot(None, Page::default()).unwrap()["outcome"]["delivered"],
            Value::Null
        );
        saved["finished"] = json!(true);
        saved["status"] = json!("delivered");
        saved["sequence"] = json!(4);
        atomic_json(&root.path().join("claude.json"), &saved).unwrap();
        let result = view.snapshot(None, Page::default()).unwrap();
        assert_eq!(result["outcome"]["delivered"], true);
        assert_eq!(result["outcome"]["verification_required"], true);
        assert_eq!(result["outcome"]["cleanup_complete"], true);
        assert!(!result.to_string().contains("PRIVATE COMMAND"));
    }

    #[test]
    fn incomplete_generated_outputs_remain_distinct_from_conflicts_and_are_bounded() {
        let paths = (0..40)
            .map(|n| json!(format!("renders/output-{n}.mp4")))
            .collect::<Vec<_>>();
        let projected = project_outcome(&json!({"status":"delivery_conflict","delivery":{
            "delivered":false,"cleanup_complete":true,"conflicts":[],"recovery":"/saved/outputs",
            "artifacts":["renders/declared.mp4"],"undelivered_outputs":paths,
            "excluded_paths":{".env":"recognized credential; not exported"}}}));
        assert_eq!(projected["delivered"], false);
        assert_eq!(projected["conflicts"], json!([]));
        assert_eq!(projected["undelivered_outputs_total"], 40);
        assert_eq!(
            projected["undelivered_outputs"].as_array().unwrap().len(),
            16
        );
        assert_eq!(projected["recovery_path"], "/saved/outputs");
        assert_eq!(projected["artifacts_total"], 1);
        assert_eq!(projected["excluded_paths"][0]["path"], ".env");
        let delivered = project_outcome(&json!({"status":"delivered","delivery":{
            "delivered":true,"artifacts_declared":true,"undelivered_outputs":["build/cache.log"]}}));
        assert_eq!(delivered["artifacts_declared"], true);
        assert_eq!(delivered["delivered"], true);
        assert_eq!(delivered["undelivered_outputs_total"], 1);
    }

    #[test]
    fn recovery_records_work_after_bridge_exit_without_opening_worker_folders() {
        let (root, board, mut saved) = fixture();
        drop(board);
        saved["finished"] = json!(true);
        saved["status"] = json!("stopped");
        saved["recovery"] = json!({"recovery":"/private/recovery","cleanup_complete":true});
        atomic_json(&root.path().join("claude.json"), &saved).unwrap();
        for name in ["baseline", "worker1", "worker2"] {
            fs::remove_dir(root.path().join(name)).unwrap();
        }
        let before = fs::read_dir(root.path().join("board"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        let result = observer(root.path())
            .snapshot(None, Page::default())
            .unwrap();
        let after = fs::read_dir(root.path().join("board"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(result["outcome"]["recovery_saved"], true);
        assert_eq!(result["outcome"]["cleanup_complete"], true);
        assert_eq!(result["outcome"]["delivered"], Value::Null);
        assert_eq!(
            before, after,
            "Read-only viewing must not create database companions"
        );
    }

    #[test]
    fn identity_links_unknown_versions_and_backwards_sequences_are_rejected() {
        let (root, _board, mut saved) = fixture();
        assert!(Observer::new(root.path().into(), "run".into(), "other-session".into()).is_err());
        let mut view = observer(root.path());
        view.snapshot(None, Page::default()).unwrap();
        saved["sequence"] = json!(2);
        atomic_json(&root.path().join("claude.json"), &saved).unwrap();
        assert!(view.snapshot(None, Page::default()).is_err());
        saved["version"] = json!(99);
        atomic_json(&root.path().join("claude.json"), &saved).unwrap();
        assert!(Observer::new(root.path().into(), "run".into(), "session".into()).is_err());
        let file = root.path().join("claude.json");
        let original = root.path().join("original.json");
        fs::rename(&file, &original).unwrap();
        symlink(&original, &file).unwrap();
        assert!(read_state(root.path(), "session").is_err());
        assert!(run_path("../../elsewhere").is_err());
    }

    #[test]
    fn journal_consumes_complete_records_once_and_ignores_private_records() {
        let (root, _board, _) = fixture();
        let mut stream = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(root.path().join("events.jsonl"))
            .unwrap();
        stream
            .write_all(b"{\"kind\":\"claude_final\",\"data\":{\"status\":\"complete\"")
            .unwrap();
        let mut journal = Journal::default();
        journal.read(root.path()).unwrap();
        assert!(journal.final_event.is_none());
        assert!(!journal.complete);
        stream
            .write_all(b",\"delivery\":{\"delivered\":true}}}\n")
            .unwrap();
        journal.read(root.path()).unwrap();
        assert!(journal.complete);
        assert_eq!(journal.final_event.as_ref().unwrap()["delivered"], true);
        let offset = journal.offset;
        journal.read(root.path()).unwrap();
        assert_eq!(journal.offset, offset);
    }

    #[test]
    fn differing_source_revisions_keep_the_last_compatible_board() {
        let (root, mut board, mut saved) = fixture();
        let mut view = observer(root.path());
        let first = view.snapshot(None, Page::default()).unwrap();
        board.set_revision(2).unwrap();
        let between = view.snapshot(None, Page::default()).unwrap();
        assert_eq!(
            between["source"]["board_revision"],
            first["source"]["board_revision"]
        );
        assert!(
            between["freshness"]["unavailable"]
                .as_array()
                .unwrap()
                .contains(&json!("board_revision"))
        );
        saved["revision"] = json!(2);
        saved["sequence"] = json!(4);
        atomic_json(&root.path().join("claude.json"), &saved).unwrap();
        let coherent = view.snapshot(None, Page::default()).unwrap();
        assert_eq!(coherent["source"]["board_revision"], 2);
        assert_eq!(coherent["revision"], 2);
        assert_eq!(
            coherent["agents"][0]["request_revision"], 1,
            "Accepted input is not worker receipt"
        );
    }

    #[test]
    fn byte_budget_keeps_complete_rows_and_contiguous_page_offsets() {
        let row = json!({"id":1,"title":"Shared contribution","text":"界".repeat(2048),
            "files":(0..32).map(|index|format!("{index}/{}", "a/".repeat(254))).collect::<Vec<_>>()});
        let items = (0..32)
            .map(|index| {
                let mut value = row.clone();
                value["id"] = json!(index + 1);
                value
            })
            .collect::<Vec<_>>();
        let mut value = json!({"tasks":{"items":[],"total":0,"offset":0,"next_offset":Value::Null},
            "checks":{"items":[],"total":0,"offset":0,"next_offset":Value::Null},
            "shared":{"items":items,"total":80,"offset":16,"limit":32,"next_offset":48}});
        assert!(serde_json::to_vec(&value).unwrap().len() > OUTPUT_LIMIT);
        fit_pages(&mut value, Some("shared")).unwrap();
        let rows = value["shared"]["items"].as_array().unwrap();
        assert!(!rows.is_empty() && rows.len() < 32);
        assert_eq!(value["shared"]["total"], 80);
        assert_eq!(value["shared"]["next_offset"], 16 + rows.len());
        assert_eq!(
            rows[0], row,
            "Byte limiting must not rewrite a retained detail row"
        );
        let encoded = serde_json::to_vec(&value).unwrap();
        assert!(encoded.len() <= PAGE_BUDGET);
        assert!(std::str::from_utf8(&encoded).is_ok());
    }

    #[test]
    fn restored_journal_tail_keeps_a_record_starting_at_its_exact_boundary() {
        let (root, _board, _) = fixture();
        let mut record = json!({"kind":"claude_final","data":{"status":"complete",
            "delivery":{"delivered":true,"cleanup_complete":true}},"padding":""});
        let base = serde_json::to_vec(&record).unwrap().len();
        record["padding"] = json!("x".repeat(RECORD_LIMIT - base - 1));
        let mut bytes = serde_json::to_vec(&record).unwrap();
        bytes.push(b'\n');
        assert_eq!(bytes.len(), RECORD_LIMIT);
        let mut stream = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.path().join("events.jsonl"))
            .unwrap();
        stream.write_all(b"{}\n").unwrap();
        stream.write_all(&bytes).unwrap();
        drop(stream);
        let mut journal = Journal::default();
        for _ in 0..RECORD_LIMIT / JOURNAL_CHUNK {
            journal.read(root.path()).unwrap();
        }
        assert!(journal.complete);
        assert_eq!(journal.final_event.as_ref().unwrap()["delivered"], true);
    }

    #[test]
    fn missing_final_evidence_is_explicitly_unavailable_not_invented() {
        let (root, _board, mut saved) = fixture();
        saved["finished"] = json!(true);
        saved["status"] = json!("complete");
        atomic_json(&root.path().join("claude.json"), &saved).unwrap();
        let value = observer(root.path())
            .snapshot(None, Page::default())
            .unwrap();
        assert_eq!(value["outcome"]["delivered"], Value::Null);
        assert!(
            value["freshness"]["unavailable"]
                .as_array()
                .unwrap()
                .contains(&json!("outcome"))
        );
    }

    fn storage_home(runs_mode: u32, run_mode: u32) -> (tempfile::TempDir, String, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("Library/Application Support/DeLM");
        let id = uuid::Uuid::new_v4().to_string();
        let run = root.join("runs").join(&id);
        fs::create_dir_all(&run).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(root.join("runs"), fs::Permissions::from_mode(runs_mode)).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(run_mode)).unwrap();
        (home, id, run)
    }

    #[test]
    fn run_storage_created_by_older_releases_remains_viewable() {
        // Older releases created `runs` with the default 0755 mode and the run
        // writer still accepts it, so the observer must open those runs too.
        for runs_mode in [0o700, 0o750, 0o755] {
            let (home, id, run) = storage_home(runs_mode, 0o700);
            let resolved = run_path_in(home.path(), &id).unwrap();
            assert_eq!(resolved, run.canonicalize().unwrap());
        }
        let (home, id, _) = storage_home(0o755, 0o700);
        fs::set_permissions(
            home.path().join("Library/Application Support/DeLM"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(run_path_in(home.path(), &id).is_ok());
    }

    #[test]
    fn run_storage_that_other_users_can_change_or_read_is_refused_with_its_reason() {
        for runs_mode in [0o775, 0o757, 0o777] {
            let (home, id, _) = storage_home(runs_mode, 0o700);
            let error = format!("{:#}", run_path_in(home.path(), &id).unwrap_err());
            assert!(
                error.contains("must not be writable by other users"),
                "{error}"
            );
            assert!(error.contains(&format!("{:03o}", runs_mode)), "{error}");
        }
        for run_mode in [0o750, 0o705, 0o755] {
            let (home, id, _) = storage_home(0o755, run_mode);
            let error = format!("{:#}", run_path_in(home.path(), &id).unwrap_err());
            assert!(
                error.contains("must not be accessible to other users"),
                "{error}"
            );
        }
    }

    #[test]
    fn linked_or_missing_run_storage_is_refused_without_creating_anything() {
        let (home, id, run) = storage_home(0o755, 0o700);
        let elsewhere = home.path().join("elsewhere");
        fs::rename(&run, &elsewhere).unwrap();
        symlink(&elsewhere, &run).unwrap();
        assert!(run_path_in(home.path(), &id).is_err());
        fs::remove_file(&run).unwrap();
        let runs = run.parent().unwrap();
        let moved = home.path().join("moved-runs");
        fs::rename(runs, &moved).unwrap();
        symlink(&moved, runs).unwrap();
        assert!(run_path_in(home.path(), &id).is_err());
        fs::remove_file(runs).unwrap();
        fs::create_dir(runs).unwrap();
        fs::set_permissions(runs, fs::Permissions::from_mode(0o755)).unwrap();
        let error = format!("{:#}", run_path_in(home.path(), &id).unwrap_err());
        assert!(error.contains("cannot open"), "{error}");
        assert!(!run.exists(), "Viewing a missing run must not create it");
        assert!(run_path_in(home.path(), "not-a-run").is_err());
    }
}
