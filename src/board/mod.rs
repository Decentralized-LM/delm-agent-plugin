//! Durable coordination for the two native workers.
//!
//! Actor identities and request revisions come from the runtime. Model arguments
//! never select a workspace or a destination. Large bodies remain in immutable
//! COW objects and are read only through explicit expansion or import.

mod files;

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use files::{FileVersion, Root};

const VIEW_LIMIT: i64 = 24;
const MAX_ARGUMENT_BYTES: usize = 128 * 1024;

pub struct Board {
    db: Connection,
    lock: File,
    objects: Root,
    baseline: Root,
    workers: [Root; 2],
    policies: [Option<Vec<(PathBuf, String)>>; 2],
}

impl Board {
    pub fn open(run_dir: &Path, baseline: &Path, workers: [PathBuf; 2]) -> Result<Self> {
        let run = fs::canonicalize(run_dir).context("open run directory")?;
        let baseline = Root::open(baseline)?;
        let workers = [Root::open(&workers[0])?, Root::open(&workers[1])?];
        ensure!(
            baseline.path != workers[0].path && baseline.path != workers[1].path,
            "baseline must be separate from workers"
        );
        ensure!(
            !workers[0].path.starts_with(&workers[1].path)
                && !workers[1].path.starts_with(&workers[0].path),
            "worker roots must be separate"
        );
        let state = run.join("board");
        for worker in &workers {
            ensure!(
                !state.starts_with(&worker.path) && !worker.path.starts_with(&state),
                "board state must be outside worker trees"
            );
            ensure!(
                !baseline.path.starts_with(&worker.path)
                    && !worker.path.starts_with(&baseline.path),
                "baseline must be outside worker trees"
            );
        }
        ensure!(
            !state.starts_with(&baseline.path) && !baseline.path.starts_with(&state),
            "board state must be outside the baseline"
        );
        create_owned_dir(&state)?;
        let lock = files::open_private_file(&state.join("lock"), true)?;
        lock.lock_exclusive()?;
        let initialized = (|| -> Result<Self> {
            let database = state.join("board.sqlite3");
            // Open the owned file without following links before SQLite sees it.
            let guard = files::open_private_file(&database, true)?;
            ensure!(
                guard.metadata()?.is_file(),
                "board database must be a regular file"
            );
            let db = Connection::open(&database)?;
            db.busy_timeout(Duration::from_secs(10))?;
            db.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=FULL;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS context (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL);
                 INSERT OR IGNORE INTO context VALUES (1,1);
                 CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY AUTOINCREMENT, worker INTEGER NOT NULL, revision INTEGER NOT NULL, kind TEXT NOT NULL, body TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS requests (worker INTEGER NOT NULL, key TEXT NOT NULL, name TEXT NOT NULL, digest TEXT NOT NULL, state TEXT NOT NULL, response TEXT, detail TEXT, PRIMARY KEY(worker,key));
                 CREATE TABLE IF NOT EXISTS tasks (id INTEGER PRIMARY KEY, author INTEGER NOT NULL, owner INTEGER, state TEXT NOT NULL, body TEXT NOT NULL, updated INTEGER NOT NULL);
                 CREATE TABLE IF NOT EXISTS workers (worker INTEGER PRIMARY KEY CHECK(worker IN (1,2)), state TEXT NOT NULL, summary TEXT NOT NULL, dependency TEXT, updated INTEGER NOT NULL);
                 INSERT OR IGNORE INTO workers VALUES (1,'working','',NULL,0),(2,'working','',NULL,0);
                 CREATE TABLE IF NOT EXISTS publications (id INTEGER PRIMARY KEY, worker INTEGER NOT NULL, revision INTEGER NOT NULL, body TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS publication_files (publication INTEGER NOT NULL REFERENCES publications(id), path TEXT NOT NULL, version TEXT NOT NULL, PRIMARY KEY(publication,path));"
            )?;
            let object_dir = state.join("objects");
            create_owned_dir(&object_dir)?;
            Ok(Self {
                db,
                lock: lock.try_clone()?,
                objects: Root::open(&object_dir)?,
                baseline,
                workers,
                policies: [None, None],
            })
        })();
        FileExt::unlock(&lock)?;
        initialized
    }

    /// Called only by the native owner when the user's request changes.
    pub fn set_revision(&mut self, revision: u64) -> Result<()> {
        ensure!(
            revision > 0 && revision <= i64::MAX as u64,
            "invalid request revision"
        );
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: u64 =
            tx.query_row("SELECT revision FROM context WHERE id=1", [], |r| r.get(0))?;
        ensure!(
            revision >= current,
            "request revision cannot move backwards"
        );
        tx.execute("UPDATE context SET revision=? WHERE id=1", [revision])?;
        tx.commit()?;
        Ok(())
    }

    pub fn view(&self) -> Result<Value> {
        view(&self.db)
    }

    pub fn dependency_owner(&self, dependency: &str) -> Result<Option<usize>> {
        dependency_owner(&self.db, dependency)
    }

    /// Bind the native worker's filesystem profile to privileged board I/O.
    pub fn set_worker_policy(&mut self, worker: usize, config: &Value) -> Result<()> {
        ensure!((1..=2).contains(&worker), "unbound worker identity");
        let profile = config["default_permissions"]
            .as_str()
            .context("missing worker permission profile")?;
        let filesystem = config["permissions"][profile]["filesystem"]
            .as_object()
            .context("missing worker filesystem profile")?;
        let mut rules = Vec::new();
        for (path, access) in filesystem {
            if path == ":minimal" {
                continue;
            }
            ensure!(
                !path.starts_with(':') && !path.contains(['*', '?', '[', ']', '{', '}']),
                "board transfer policy requires literal paths; unsupported glob or special rule: {path}"
            );
            let path = PathBuf::from(path);
            ensure!(
                path.is_absolute()
                    && !path
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir)),
                "board permission paths must be absolute"
            );
            let access = access.as_str().context("invalid board permission access")?;
            ensure!(
                ["read", "write", "deny"].contains(&access),
                "unknown board permission access"
            );
            let path = files::normalize_rule_path(&path)?;
            rules.push((path, access.to_owned()));
        }
        self.policies[worker - 1] = Some(rules);
        Ok(())
    }

    fn require_access(&self, worker: usize, relative: &str, write: bool) -> Result<()> {
        files::validate_relative(relative)?;
        let rules = self.policies[worker - 1]
            .as_ref()
            .context("worker transfer permissions have not been bound")?;
        let path = self.workers[worker - 1].path.join(relative);
        for (scope, access) in rules {
            if access != "write" {
                ensure!(
                    !files::restrictive_alias(&path, scope)?,
                    "worker policy forbids a case or Unicode alias of a restrictive path: {relative}"
                );
            }
        }
        let access = rules
            .iter()
            .filter(|(scope, _)| path.starts_with(scope))
            .max_by_key(|(scope, access)| {
                (
                    scope.components().count(),
                    match access.as_str() {
                        "deny" => 3,
                        "write" => 2,
                        _ => 1,
                    },
                )
            })
            .map(|(_, access)| access.as_str())
            .unwrap_or("deny");
        ensure!(
            access == "write" || (!write && access == "read"),
            "worker policy forbids {} of {relative}",
            if write { "import" } else { "publication read" }
        );
        Ok(())
    }

    pub fn call(&mut self, worker: usize, name: &str, args: Value) -> Result<Value> {
        ensure!((1..=2).contains(&worker), "unbound worker identity");
        ensure!(args.is_object(), "tool arguments must be an object");
        for forbidden in [
            "worker",
            "worker_id",
            "agent_id",
            "author",
            "attempt",
            "revision",
            "destination",
        ] {
            ensure!(
                args.get(forbidden).is_none(),
                "{forbidden} is runtime-owned, not a tool argument"
            );
        }
        ensure!(
            serde_json::to_vec(&args)?.len() <= MAX_ARGUMENT_BYTES,
            "coordination metadata exceeds 128 KiB; publish files and expand selectively"
        );
        self.lock.lock_exclusive()?;
        let result = self.call_locked(worker, name, &args);
        let unlocked = FileExt::unlock(&self.lock);
        match result {
            Ok(value) => {
                unlocked?;
                Ok(value)
            }
            Err(error) => {
                let _ = unlocked;
                Err(error)
            }
        }
    }

    fn call_locked(&mut self, worker: usize, name: &str, args: &Value) -> Result<Value> {
        self.workers[worker - 1].verify()?;
        let mutation = name != "delm_expand"
            && (name != "delm_status"
                || ["state", "summary", "dependency", "finding"]
                    .iter()
                    .any(|key| args.get(key).is_some()));
        if !mutation {
            let result = match name {
                "delm_status" => json!({"worker": worker}),
                "delm_expand" => self.expand(worker, args)?,
                _ => bail!("unknown coordination tool: {name}"),
            };
            return Ok(json!({"result": result, "board": self.view()?}));
        }
        let key = string(args, "idempotency_key", 128)?;
        if name == "delm_complete" {
            let revision: u64 =
                self.db
                    .query_row("SELECT revision FROM context WHERE id=1", [], |r| r.get(0))?;
            ensure!(
                args.get("expected_revision").and_then(Value::as_u64) == Some(revision),
                "completion must acknowledge the current board request_revision; this declaration is stale"
            );
        }
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(args)?));
        let prior = self
            .db
            .query_row(
                "SELECT name,digest,state,response,detail FROM requests WHERE worker=? AND key=?",
                params![worker, key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some((previous_name, previous_digest, state, response, detail)) = prior {
            ensure!(
                previous_name == name && previous_digest == digest,
                "idempotency key was already used with different arguments"
            );
            if state == "done" {
                return Ok(serde_json::from_str(
                    &response.context("missing durable response")?,
                )?);
            }
            ensure!(name == "delm_apply", "unknown previous mutation outcome");
            return self.reconcile_apply(
                worker,
                key,
                detail.as_deref().context("missing import journal")?,
            );
        }
        if name == "delm_apply" {
            return self.apply(worker, key, &digest, args);
        }
        // File freezing precedes the database transaction. An interrupted freeze
        // can leave an unreferenced owned object, but never a visible partial publication.
        let frozen = if name == "delm_publish" {
            Some(self.freeze(worker, args)?)
        } else {
            None
        };
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = match name {
            "delm_status" => status(&tx, worker, args)?,
            "delm_task_create" => task_create(&tx, worker, args)?,
            "delm_task_claim" => task_claim(&tx, worker, args)?,
            "delm_task_finish" => task_finish(&tx, worker, args)?,
            "delm_publish" => publish(&tx, worker, frozen.context("missing frozen publication")?)?,
            "delm_complete" => complete(&tx, worker, args)?,
            _ => bail!("unknown coordination tool: {name}"),
        };
        let response = json!({"result": result, "board": view(&tx)?});
        tx.execute(
            "INSERT INTO requests(worker,key,name,digest,state,response) VALUES (?,?,?,?,'done',?)",
            params![worker, key, name, digest, serde_json::to_string(&response)?],
        )?;
        tx.commit()?;
        Ok(response)
    }

    fn freeze(&self, worker: usize, args: &Value) -> Result<Value> {
        let summary = string(args, "summary", 2048)?;
        let paths = paths(args, "paths", false)?;
        let large = args
            .get("large_artifact")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let size_limit = if large {
            4 * 1024 * 1024 * 1024u64
        } else {
            64 * 1024 * 1024u64
        };
        ensure!(
            paths.len() <= if large { 16384 } else { 1024 },
            "publication manifest is too large; split the contribution or use large_artifact"
        );
        let dependencies = dependencies(&self.db, args)?;
        let mut bytes = 0u64;
        let mut manifests = Vec::new();
        for path in paths {
            self.require_access(worker, &path, false)?;
            let base = self.baseline.version(&path)?;
            let source = self.workers[worker - 1].version(&path)?;
            bytes = bytes
                .checked_add(source.as_ref().map(|v| v.bytes).unwrap_or(0))
                .context("publication size overflow")?;
            ensure!(
                bytes <= size_limit,
                "publication exceeds transport size limit; use large_artifact for up to 4 GiB or split into selective publications"
            );
            let (result, object) = if let Some(expected) = source {
                let object = uuid::Uuid::new_v4().to_string();
                let frozen =
                    self.workers[worker - 1].freeze(&path, &self.objects, &object, &expected)?;
                (Some(frozen), Some(object))
            } else {
                (None, None)
            };
            manifests.push(json!({"path": path, "base": base, "result": result, "object": object}));
        }
        Ok(
            json!({"summary": summary, "files": manifests, "dependencies": dependencies,
            "interfaces": optional_string(args, "interfaces", 8192)?,
            "unfinished": optional_string(args, "unfinished", 8192)?,
            "checks": checks(args)?, "bytes": bytes,
            "author_attempt": 1}),
        )
    }

    fn publication(&self, id: i64) -> Result<Value> {
        let body = self
            .db
            .query_row("SELECT body FROM publications WHERE id=?", [id], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .context("unknown publication")?;
        Ok(serde_json::from_str(&body)?)
    }

    fn expand(&self, worker: usize, args: &Value) -> Result<Value> {
        let targets = ["publication_id", "task_id", "finding_id"]
            .iter()
            .filter(|key| args.get(**key).is_some())
            .count();
        ensure!(
            targets == 1,
            "expand exactly one publication_id, task_id, or finding_id"
        );
        if args.get("task_id").is_some() {
            let id = positive_id(args, "task_id")?;
            let (body, owner, state): (String, Option<usize>, String) = self
                .db
                .query_row("SELECT body,owner,state FROM tasks WHERE id=?", [id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?
                .context("unknown task")?;
            let mut body: Value = serde_json::from_str(&body)?;
            body["task_id"] = json!(id);
            body["owner"] = json!(owner);
            body["state"] = json!(state);
            return Ok(body);
        }
        if args.get("finding_id").is_some() {
            let id = positive_id(args, "finding_id")?;
            let body: String = self
                .db
                .query_row(
                    "SELECT body FROM events WHERE seq=? AND kind='finding'",
                    [id],
                    |r| r.get(0),
                )
                .optional()?
                .context("unknown finding")?;
            return Ok(serde_json::from_str(&body)?);
        }
        let id = positive_id(args, "publication_id")?;
        let publication = self.publication(id)?;
        let chosen = select_files(&publication, args)?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let max_bytes = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(8192)
            .min(32768);
        ensure!(
            chosen.len() <= 32,
            "expand at most 32 selected files at a time"
        );
        let mut remaining = 65536u64;
        let mut expanded = Vec::new();
        for mut file in chosen {
            self.require_access(
                worker,
                file["path"].as_str().context("invalid publication path")?,
                false,
            )?;
            if let Some(object) = file["object"].as_str() {
                let expected: FileVersion = serde_json::from_value(file["result"].clone())?;
                let bytes =
                    self.objects
                        .read_range(object, &expected, offset, max_bytes.min(remaining))?;
                remaining = remaining.saturating_sub(bytes.len() as u64);
                file["offset"] = json!(offset);
                file["returned_bytes"] = json!(bytes.len());
                file["total_bytes"] = json!(expected.bytes);
                match String::from_utf8(bytes) {
                    Ok(text) => file["text"] = json!(text),
                    Err(_) => {
                        file["binary"] = json!(true);
                        file["note"] = json!(
                            "Selected bytes are not UTF-8; apply imports the exact immutable file."
                        );
                    }
                }
            }
            file.as_object_mut().unwrap().remove("object");
            expanded.push(file);
        }
        let mut result = publication;
        result["files"] = json!(expanded);
        Ok(result)
    }

    fn apply(&mut self, worker: usize, key: &str, digest: &str, args: &Value) -> Result<Value> {
        let id = positive_id(args, "publication_id")?;
        let publication = self.publication(id)?;
        let mut selected = select_files(&publication, args)?;
        ensure!(!selected.is_empty(), "publication has no selected files");
        let mut conflicts = Vec::new();
        for file in &mut selected {
            let path = file["path"].as_str().context("invalid publication path")?;
            self.require_access(worker, path, true)?;
            let local = self.workers[worker - 1].version(path)?;
            let target: Option<FileVersion> = serde_json::from_value(file["result"].clone())?;
            let base = self.baseline.version(path)?;
            if local != base && local != target && !self.known_version(path, &local)? {
                conflicts.push(path.to_owned());
            }
            if let Some(object) = file["object"].as_str() {
                self.objects
                    .verify_object(object, target.as_ref().context("missing object manifest")?)?;
            }
            file["before"] = serde_json::to_value(local)?;
        }
        ensure!(
            conflicts.is_empty(),
            "local changes diverged from baseline and published versions: {}. Reconcile these files explicitly before importing.",
            conflicts.join(", ")
        );
        let detail = json!({"publication_id": id, "files": selected});
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO requests(worker,key,name,digest,state,detail) VALUES (?,?,'delm_apply',?,'applying',?)",
            params![worker, key, digest, serde_json::to_string(&detail)?])?;
        tx.commit()?;
        for file in detail["files"].as_array().unwrap() {
            let path = file["path"].as_str().unwrap();
            let expected: Option<FileVersion> = serde_json::from_value(file["result"].clone())?;
            let before: Option<FileVersion> = serde_json::from_value(file["before"].clone())?;
            self.workers[worker - 1].install(path, &self.objects, file["object"].as_str(), expected.as_ref(), before.as_ref())
                .with_context(|| format!("import outcome is unknown at {path}; inspect the worker tree and retry the same idempotency key to reconcile"))?;
        }
        self.reconcile_apply(worker, key, &serde_json::to_string(&detail)?)
    }

    fn known_version(&self, path: &str, version: &Option<FileVersion>) -> Result<bool> {
        let encoded = serde_json::to_string(version)?;
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM publication_files WHERE path=? AND version=?)",
            params![path, encoded],
            |r| r.get(0),
        )?)
    }

    fn reconcile_apply(&mut self, worker: usize, key: &str, detail: &str) -> Result<Value> {
        let detail: Value = serde_json::from_str(detail)?;
        let mut imported = Vec::new();
        for file in detail["files"]
            .as_array()
            .context("invalid import journal")?
        {
            let path = file["path"]
                .as_str()
                .context("invalid import journal path")?;
            self.require_access(worker, path, true)?;
            let expected: Option<FileVersion> = serde_json::from_value(file["result"].clone())?;
            ensure!(
                self.workers[worker - 1].version(path)? == expected,
                "previous import outcome is unknown or local files changed at {path}; no success is recorded and no additional files were written"
            );
            imported.push(path.to_owned());
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = json!({"publication_id": detail["publication_id"], "paths": imported, "confirmed": true});
        event(&tx, worker, "apply", &result)?;
        let response = json!({"result": result, "board": view(&tx)?});
        tx.execute(
            "UPDATE requests SET state='done',response=? WHERE worker=? AND key=?",
            params![serde_json::to_string(&response)?, worker, key],
        )?;
        tx.commit()?;
        Ok(response)
    }
}

fn create_owned_dir(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "board state path must be a real directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn event(tx: &Transaction<'_>, worker: usize, kind: &str, body: &Value) -> Result<i64> {
    tx.execute("INSERT INTO events(worker,revision,kind,body) SELECT ?,revision,?,? FROM context WHERE id=1",
        params![worker, kind, serde_json::to_string(body)?])?;
    Ok(tx.last_insert_rowid())
}

fn status(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let current: (String, String, Option<String>) = tx.query_row(
        "SELECT state,summary,dependency FROM workers WHERE worker=?",
        [worker],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let state = args
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or(&current.0);
    ensure!(
        ["working", "waiting", "blocked"].contains(&state),
        "invalid worker status"
    );
    let summary = if args.get("summary").is_some() {
        optional_string(args, "summary", 2048)?
    } else {
        current.1
    };
    let dependency = if args.get("dependency").is_some() {
        Some(string(args, "dependency", 512)?.to_owned())
    } else {
        current.2
    };
    ensure!(
        state != "waiting" || dependency.as_ref().is_some_and(|s| !s.is_empty()),
        "waiting requires a named dependency"
    );
    let dependency = if state == "waiting" { dependency } else { None };
    let body =
        json!({"worker": worker, "state": state, "summary": summary, "dependency": dependency});
    let seq = event(tx, worker, "status", &body)?;
    tx.execute(
        "UPDATE workers SET state=?,summary=?,dependency=?,updated=? WHERE worker=?",
        params![state, summary, dependency, seq, worker],
    )?;
    if args.get("finding").is_some() {
        event(
            tx,
            worker,
            "finding",
            &json!({"text": string(args, "finding", 4096)?}),
        )?;
    }
    Ok(body)
}

fn task_create(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let body = json!({"title": string(args, "title", 256)?, "description": string(args, "description", 2048)?,
        "interface": optional_string(args, "interface", 2048)?, "dependencies": dependencies(tx, args)?,
        "earliest_contribution": optional_string(args, "earliest_contribution", 1024)?,
        "done_when": optional_string(args, "done_when", 1024)?});
    let id = event(tx, worker, "task_create", &body)?;
    tx.execute(
        "INSERT INTO tasks VALUES (?,?,NULL,'available',?,?)",
        params![id, worker, serde_json::to_string(&body)?, id],
    )?;
    Ok(json!({"task_id": id, "state": "available"}))
}

fn task_claim(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let id = positive_id(args, "task_id")?;
    let task: Option<(Option<usize>, String)> = tx
        .query_row("SELECT owner,state FROM tasks WHERE id=?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let (owner, state) = task.context("unknown task")?;
    ensure!(
        state == "available" || (state == "claimed" && owner == Some(worker)),
        "task is already owned by another worker or finished"
    );
    let result = json!({"task_id": id, "owner": worker, "state": "claimed"});
    let seq = event(tx, worker, "task_claim", &result)?;
    tx.execute("UPDATE tasks SET owner=?,state='claimed',updated=? WHERE id=? AND (owner IS NULL OR owner=?)",
        params![worker, seq, id, worker])?;
    Ok(result)
}

fn task_finish(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let id = positive_id(args, "task_id")?;
    let (owner, state): (Option<usize>, String) = tx
        .query_row("SELECT owner,state FROM tasks WHERE id=?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?
        .context("unknown task")?;
    ensure!(
        owner == Some(worker) && state == "claimed",
        "only the owner may finish a claimed task"
    );
    let result = json!({"task_id": id, "state": "done", "summary": string(args, "summary", 2048)?, "whole_task_complete": false});
    let seq = event(tx, worker, "task_finish", &result)?;
    tx.execute(
        "UPDATE tasks SET state='done',updated=? WHERE id=?",
        params![seq, id],
    )?;
    Ok(result)
}

fn publish(tx: &Transaction<'_>, worker: usize, mut body: Value) -> Result<Value> {
    let revision: u64 =
        tx.query_row("SELECT revision FROM context WHERE id=1", [], |r| r.get(0))?;
    let id = event(
        tx,
        worker,
        "publication",
        &json!({"summary": body["summary"], "file_count": body["files"].as_array().unwrap().len()}),
    )?;
    body["publication_id"] = json!(id);
    body["worker"] = json!(worker);
    body["revision"] = json!(revision);
    tx.execute(
        "INSERT INTO publications VALUES (?,?,?,?)",
        params![id, worker, revision, serde_json::to_string(&body)?],
    )?;
    for file in body["files"].as_array().unwrap() {
        // Canonical struct serialization matches the compatibility lookup.
        let version: Option<FileVersion> = serde_json::from_value(file["result"].clone())?;
        tx.execute(
            "INSERT INTO publication_files VALUES (?,?,?)",
            params![
                id,
                file["path"].as_str().unwrap(),
                serde_json::to_string(&version)?
            ],
        )?;
    }
    Ok(
        json!({"publication_id": id, "summary": body["summary"], "file_count": body["files"].as_array().unwrap().len()}),
    )
}

fn complete(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let revision: u64 =
        tx.query_row("SELECT revision FROM context WHERE id=1", [], |r| r.get(0))?;
    ensure!(
        args.get("expected_revision").and_then(Value::as_u64) == Some(revision),
        "completion must acknowledge the current board request_revision; this declaration is stale"
    );
    let outcome = string(args, "outcome", 16)?;
    ensure!(
        ["complete", "partial", "blocked", "waiting"].contains(&outcome),
        "invalid completion outcome"
    );
    let dependency = optional_string(args, "dependency", 512)?;
    ensure!(
        outcome != "waiting" || !dependency.is_empty(),
        "waiting requires a named dependency"
    );
    if outcome == "waiting" {
        let owner = dependency_owner(tx, &dependency)?;
        ensure!(
            owner != Some(worker),
            "waiting dependency belongs to this worker; continue that work instead"
        );
        if let Some(id) = dependency.strip_prefix("task:") {
            let state: String = tx.query_row(
                "SELECT state FROM tasks WHERE id=?",
                [id.parse::<i64>()?],
                |r| r.get(0),
            )?;
            ensure!(
                state != "done",
                "named task is already finished; read its contribution instead of waiting"
            );
        }
    }
    let body = json!({"outcome": outcome, "summary": string(args, "summary", 4096)?, "checks": checks(args)?, "dependency": dependency,
        "declaration_only": true,"expected_revision":revision});
    let seq = event(tx, worker, "declaration", &body)?;
    let state = match outcome {
        "waiting" => "waiting",
        "blocked" => "blocked",
        _ => "working",
    };
    tx.execute(
        "UPDATE workers SET state=?,summary=?,dependency=?,updated=? WHERE worker=?",
        params![
            state,
            body["summary"].as_str(),
            if dependency.is_empty() {
                None
            } else {
                Some(dependency)
            },
            seq,
            worker
        ],
    )?;
    Ok(body)
}

fn dependency_owner(db: &Connection, dependency: &str) -> Result<Option<usize>> {
    let (kind, id) = dependency
        .split_once(':')
        .context("waiting dependency must be task:<id> or worker:<1|2>")?;
    ensure!(
        !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid waiting dependency ID"
    );
    let id = id.parse::<i64>().context("invalid waiting dependency ID")?;
    ensure!(id > 0, "waiting dependency ID must be positive");
    match kind {
        "worker" => {
            ensure!((1..=2).contains(&id), "waiting worker must be 1 or 2");
            Ok(Some(id as usize))
        }
        "task" => db
            .query_row("SELECT owner FROM tasks WHERE id=?", [id], |r| {
                r.get::<_, Option<usize>>(0)
            })
            .optional()?
            .context("unknown waiting task"),
        _ => bail!("waiting dependency must be task:<id> or worker:<1|2>"),
    }
}

fn view(db: &Connection) -> Result<Value> {
    let revision: u64 =
        db.query_row("SELECT revision FROM context WHERE id=1", [], |r| r.get(0))?;
    let sequence: i64 =
        db.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))?;
    let mut tasks_query = db.prepare(
        "SELECT id,author,owner,state,body FROM tasks ORDER BY (state='done'),updated DESC LIMIT ?",
    )?;
    let tasks = tasks_query.query_map([VIEW_LIMIT], |r| Ok((r.get::<_, i64>(0)?,r.get::<_, usize>(1)?,r.get::<_, Option<usize>>(2)?,r.get::<_, String>(3)?,r.get::<_, String>(4)?)))?
        .map(|row| -> Result<Value> { let (id,author,owner,state,body) = row?; let body: Value=serde_json::from_str(&body)?; Ok(json!({"task_id":id,"author":author,"owner":owner,"state":state,"title":body["title"],"description":compact(&body["description"],256),"interface":compact(&body["interface"],256),"dependencies":body["dependencies"]})) }).collect::<Result<Vec<_>>>()?;
    let mut workers_query =
        db.prepare("SELECT worker,state,summary,dependency,updated FROM workers ORDER BY worker")?;
    let workers=workers_query.query_map([], |r| Ok(json!({"worker":r.get::<_,usize>(0)?,"state":r.get::<_,String>(1)?,"summary":r.get::<_,String>(2)?,"dependency":r.get::<_,Option<String>>(3)?,"sequence":r.get::<_,i64>(4)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let mut publication_query =
        db.prepare("SELECT id,worker,revision,body FROM publications ORDER BY id DESC LIMIT ?")?;
    let publications=publication_query.query_map([VIEW_LIMIT], |r|Ok((r.get::<_,i64>(0)?,r.get::<_,usize>(1)?,r.get::<_,u64>(2)?,r.get::<_,String>(3)?)))?
        .map(|row| -> Result<Value> {let(id,worker,revision,body)=row?;let body:Value=serde_json::from_str(&body)?;Ok(json!({"publication_id":id,"worker":worker,"revision":revision,"summary":body["summary"],"files":body["files"].as_array().unwrap().len(),"dependencies":body["dependencies"],"unfinished":compact(&body["unfinished"],512)}))}).collect::<Result<Vec<_>>>()?;
    let mut findings_query = db.prepare(
        "SELECT seq,worker,body FROM events WHERE kind='finding' ORDER BY seq DESC LIMIT ?",
    )?;
    let findings = findings_query
        .query_map([VIEW_LIMIT], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, usize>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .map(|row| -> Result<Value> {
            let (seq, worker, body) = row?;
            let body: Value = serde_json::from_str(&body)?;
            Ok(json!({"sequence":seq,"worker":worker,"text":compact(&body["text"],1024)}))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"sequence":sequence,"request_revision":revision,"workers":workers,"tasks":tasks,"publications":publications,"findings":findings,"view_limit":VIEW_LIMIT}),
    )
}

fn compact(value: &Value, max: usize) -> String {
    let text = value.as_str().unwrap_or("");
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        format!("{}...", text.chars().take(max).collect::<String>())
    }
}

fn string<'a>(args: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("{key} must be a string"))?;
    ensure!(
        !value.trim().is_empty() && value.len() <= max,
        "{key} must be nonempty and at most {max} bytes"
    );
    Ok(value)
}

fn optional_string(args: &Value, key: &str, max: usize) -> Result<String> {
    match args.get(key) {
        None => Ok(String::new()),
        Some(Value::String(value)) if value.len() <= max => Ok(value.clone()),
        _ => bail!("{key} must be a string of at most {max} bytes"),
    }
}

fn positive_id(args: &Value, key: &str) -> Result<i64> {
    let id = args
        .get(key)
        .and_then(Value::as_i64)
        .with_context(|| format!("{key} must be a positive integer"))?;
    ensure!(id > 0, "{key} must be positive");
    Ok(id)
}

fn paths(args: &Value, key: &str, optional: bool) -> Result<Vec<String>> {
    let Some(value) = args.get(key) else {
        ensure!(optional, "{key} is required");
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .with_context(|| format!("{key} must be an array"))?;
    let mut result = Vec::new();
    let mut unique = std::collections::BTreeSet::new();
    for value in values {
        let path = value.as_str().context("paths must be strings")?;
        files::validate_relative(path)?;
        ensure!(
            unique.insert(path.to_owned()),
            "duplicate path in selection"
        );
        result.push(path.to_owned());
    }
    Ok(result)
}

fn dependencies(db: &Connection, args: &Value) -> Result<Vec<i64>> {
    let Some(value) = args.get("dependencies") else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .context("dependencies must be publication IDs")?;
    ensure!(values.len() <= 128, "too many publication dependencies");
    let mut result = Vec::new();
    for value in values {
        let id = value
            .as_i64()
            .context("dependency must be a publication ID")?;
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM publications WHERE id=?)",
            [id],
            |r| r.get(0),
        )?;
        ensure!(exists, "unknown dependency publication {id}");
        result.push(id);
    }
    Ok(result)
}

fn checks(args: &Value) -> Result<Value> {
    let checks = args.get("checks").cloned().unwrap_or_else(|| json!([]));
    ensure!(
        checks.as_array().is_some_and(|items| items.len() <= 64),
        "checks must be an array of at most 64 command IDs or records"
    );
    ensure!(
        serde_json::to_vec(&checks)?.len() <= 32768,
        "check references are too large; publish output files"
    );
    Ok(checks)
}

fn select_files(publication: &Value, args: &Value) -> Result<Vec<Value>> {
    let selection = paths(args, "paths", true)?;
    let files = publication["files"]
        .as_array()
        .context("invalid publication manifest")?;
    if args.get("paths").is_none() {
        return Ok(files.clone());
    }
    selection
        .into_iter()
        .map(|path| {
            files
                .iter()
                .find(|file| file["path"].as_str() == Some(&path))
                .cloned()
                .with_context(|| format!("{path} is not in publication"))
        })
        .collect()
}

/// DynamicToolSpec values sent to the pinned native app-server.
pub fn tool_definitions() -> Vec<Value> {
    let text = json!({"type":"string"});
    let strings = json!({"type":"array","items":{"type":"string"}});
    let ids = json!({"type":"array","items":{"type":"integer","minimum":1}});
    let mut definitions = Vec::new();
    let mut define = |name: &str, description: &str, properties: Value, required: &[&str]| {
        definitions.push(json!({"type":"function","name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"deferLoading":false}));
    };
    define(
        "delm_status",
        "Read compact board. Optionally update your status or append a useful finding; updates require a unique idempotency_key. Waiting names a concrete dependency.",
        json!({"idempotency_key":text,"state":{"enum":["working","waiting","blocked"]},"summary":text,"dependency":text,"finding":text}),
        &[],
    );
    define(
        "delm_task_create",
        "Publish a brief complementary implementation contribution. Both workers remain responsible for the whole request.",
        json!({"idempotency_key":text,"title":text,"description":text,"interface":text,"dependencies":ids,"earliest_contribution":text,"done_when":text}),
        &["idempotency_key", "title", "description"],
    );
    define(
        "delm_task_claim",
        "Exclusively claim an available implementation contribution as the authenticated worker.",
        json!({"idempotency_key":text,"task_id":{"type":"integer","minimum":1}}),
        &["idempotency_key", "task_id"],
    );
    define(
        "delm_task_finish",
        "Finish your claimed contribution. This does not finish the whole request; continue useful integration or implementation.",
        json!({"idempotency_key":text,"task_id":{"type":"integer","minimum":1},"summary":text}),
        &["idempotency_key", "task_id", "summary"],
    );
    define(
        "delm_publish",
        "Freeze selected project-relative files or deletions as an immutable contribution. Publish useful partial work early; paths may be empty for an interface or finding. Large artifacts use large_artifact=true (4 GiB limit, otherwise 64 MiB).",
        json!({"idempotency_key":text,"summary":text,"paths":strings,"dependencies":ids,"interfaces":text,"unfinished":text,"checks":{"type":"array","items":{}},"large_artifact":{"type":"boolean"}}),
        &["idempotency_key", "summary", "paths"],
    );
    define(
        "delm_expand",
        "Inspect exactly one publication_id, task_id, or finding_id. Select publication paths and use offset/max_bytes for large text. Binary assets are imported losslessly with delm_apply.",
        json!({"publication_id":{"type":"integer","minimum":1},"task_id":{"type":"integer","minimum":1},"finding_id":{"type":"integer","minimum":1},"paths":strings,"offset":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":1,"maximum":32768}}),
        &[],
    );
    define(
        "delm_apply",
        "Import selected whole files into your private project. Diverged local edits are refused. Compatible means the current file equals the baseline or a published version. Check integration behavior after importing.",
        json!({"idempotency_key":text,"publication_id":{"type":"integer","minimum":1},"paths":strings}),
        &["idempotency_key", "publication_id"],
    );
    define(
        "delm_complete",
        "Declare the whole request outcome with current board request_revision as expected_revision and native command IDs for performed checks, then end your turn. Complete means the full result is ready. Waiting requires dependency task:<id> or worker:<other worker>; it cannot name your own work.",
        json!({"idempotency_key":text,"expected_revision":{"type":"integer","minimum":1},"outcome":{"enum":["complete","partial","blocked","waiting"]},"summary":text,"checks":{"type":"array","items":{}},"dependency":{"type":"string","description":"For waiting, task:<positive task ID> or worker:<1|2>; use the other worker, not yourself."}}),
        &["idempotency_key", "expected_revision", "outcome", "summary"],
    );
    definitions
}

#[cfg(test)]
mod tests;
