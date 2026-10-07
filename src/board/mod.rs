//! Durable coordination for the two native workers.
//!
//! Actor identities and request revisions come from the runtime. Model arguments
//! never select a workspace or a destination. Large bodies remain in immutable
//! COW objects and are read only through explicit expansion or import.

mod checks;
pub(crate) use checks::CompletionChecks;
mod discovery;
mod files;
pub(crate) mod reader;

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::evidence::{FilesystemAccess, FilesystemScope};
use files::{FileVersion, Root};

const VIEW_LIMIT: i64 = 24;
const MAX_ARGUMENT_BYTES: usize = 128 * 1024;

/// Runtime-only cursor taken when a native worker declares a dependency wait.
/// It is never added to the model-visible board or tool response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WaitCursor {
    sequence: i64,
    dependency: String,
}

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
                 CREATE TABLE IF NOT EXISTS publication_files (publication INTEGER NOT NULL REFERENCES publications(id), path TEXT NOT NULL, version TEXT NOT NULL, PRIMARY KEY(publication,path));
                 CREATE TABLE IF NOT EXISTS check_snapshots (id INTEGER PRIMARY KEY, worker INTEGER NOT NULL, revision INTEGER NOT NULL, body TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS check_receipts (id INTEGER PRIMARY KEY, worker INTEGER NOT NULL, revision INTEGER NOT NULL, body TEXT NOT NULL);"
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

    pub(crate) fn wait_cursor(&self, declaration: &Value) -> Result<Option<WaitCursor>> {
        if declaration["outcome"] != "waiting" {
            return Ok(None);
        }
        let dependency = string(declaration, "dependency", 512)?.to_owned();
        dependency_owner(&self.db, &dependency)?;
        let sequence = self
            .db
            .query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |r| r.get(0))?;
        Ok(Some(WaitCursor {
            sequence,
            dependency,
        }))
    }

    /// Reconcile durable readiness, including events that arrived after the
    /// declaration but before native turn completion. Other task completions
    /// and unrelated publications do not resolve a named task dependency.
    pub(crate) fn wait_ready(&self, cursor: &WaitCursor) -> Result<bool> {
        let available: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE state='available' AND
             (updated>?1 OR (json_extract(body,'$.kind')='integration' AND EXISTS(
               SELECT 1 FROM tasks WHERE state='done' AND updated>?1 AND json_extract(body,'$.kind')='integration'))) AND
             (json_extract(body,'$.kind')!='integration' OR NOT EXISTS(
               SELECT 1 FROM tasks WHERE state='claimed' AND json_extract(body,'$.kind')='integration')))",
            [cursor.sequence], |r| r.get(0))?;
        if available {
            return Ok(true);
        }
        let (kind, id) = cursor
            .dependency
            .split_once(':')
            .context("Invalid saved dependency")?;
        let id: i64 = id.parse()?;
        match kind {
            "task" => self.db.query_row(
                "SELECT state='done' AND updated>? FROM tasks WHERE id=?",
                params![cursor.sequence, id], |r| r.get(0)).map_err(Into::into),
            "worker" => self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM publications WHERE worker=? AND id>? AND revision=(SELECT revision FROM context WHERE id=1))",
                params![id, cursor.sequence], |r| r.get(0)).map_err(Into::into),
            _ => bail!("Invalid saved dependency"),
        }
    }

    /// Native lifecycle recovery only, after the owner's turn has stopped.
    /// This never claims that unpublished partial files were transferred.
    pub fn release_worker_claims(&mut self, worker: usize, reason: &str) -> Result<Vec<i64>> {
        ensure!((1..=2).contains(&worker), "unbound worker identity");
        ensure!(
            !reason.is_empty() && reason.len() <= 2048,
            "invalid release reason"
        );
        self.lock.lock_exclusive()?;
        let result = (|| {
            let tx = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let owned = {
                let mut query = tx.prepare(
                    "SELECT id,body FROM tasks WHERE owner=? AND state='claimed' ORDER BY id",
                )?;
                query
                    .query_map([worker], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            let mut released = Vec::new();
            for (id, encoded) in owned {
                let mut body: Value = serde_json::from_str(&encoded)?;
                body["handoff"] = json!(format!(
                    "Owner stopped: {reason}. Reuse published contributions; unpublished files have not been imported."
                ));
                let seq = event(
                    &tx,
                    worker,
                    "task_owner_stopped",
                    &json!({"task_id":id,"reason":reason}),
                )?;
                tx.execute(
                    "UPDATE tasks SET owner=NULL,state='available',body=?,updated=? WHERE id=?",
                    params![serde_json::to_string(&body)?, seq, id],
                )?;
                released.push(id);
            }
            tx.commit()?;
            Ok(released)
        })();
        let unlock = FileExt::unlock(&self.lock);
        if result.is_ok() {
            unlock?;
        }
        result
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
        let mut scopes = Vec::new();
        for (path, access) in filesystem {
            if path == ":minimal" {
                continue;
            }
            ensure!(
                !path.starts_with(':') && !path.contains(['*', '?', '[', ']', '{', '}']),
                "board transfer policy requires literal paths; unsupported glob or special rule: {path}"
            );
            let access = match access.as_str() {
                Some("read") => FilesystemAccess::Read,
                Some("write") => FilesystemAccess::Write,
                Some("deny") => FilesystemAccess::Deny,
                _ => bail!("unknown board permission access"),
            };
            scopes.push(FilesystemScope {
                path: PathBuf::from(path),
                access,
            });
        }
        self.set_worker_scopes(worker, scopes).map(|_| ())
    }

    /// Bind effective project scopes supplied by a native host adapter. An
    /// empty scope grants no board reads or writes; paths are never inferred.
    /// Return the resolved rules so recovery can preserve their exact meaning.
    pub fn set_worker_scopes(
        &mut self,
        worker: usize,
        scopes: Vec<FilesystemScope>,
    ) -> Result<Vec<FilesystemScope>> {
        ensure!((1..=2).contains(&worker), "unbound worker identity");
        let mut rules = Vec::new();
        let mut effective = Vec::new();
        for scope in scopes {
            let text = scope.path.to_string_lossy();
            ensure!(
                !text.contains(['*', '?', '[', ']', '{', '}']),
                "board transfer policy requires literal paths; unsupported glob rule: {text}"
            );
            ensure!(
                scope.path.is_absolute()
                    && !scope
                        .path
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir)),
                "board permission paths must be absolute"
            );
            let path = files::normalize_rule_path(&scope.path)?;
            rules.push((path.clone(), scope.access.as_str().to_owned()));
            effective.push(FilesystemScope {
                path,
                access: scope.access,
            });
        }
        self.policies[worker - 1] = Some(rules);
        Ok(effective)
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
        let mutation = !matches!(name, "delm_expand" | "delm_list")
            && (name != "delm_status"
                || ["state", "summary", "dependency", "finding"]
                    .iter()
                    .any(|key| args.get(key).is_some()));
        if !mutation {
            let result = match name {
                "delm_status" => json!({"worker": worker}),
                "delm_expand" => self.expand(worker, args)?,
                "delm_list" => discovery::list(&self.db, worker, args)?,
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
        if name == "delm_complete" {
            let revision =
                self.db
                    .query_row("SELECT revision FROM context WHERE id=1", [], |r| r.get(0))?;
            self.shared_checks(worker, args, revision)?;
        }
        // File freezing precedes the database transaction. An interrupted freeze
        // can leave an unreferenced owned object, but never a visible partial publication.
        let captured = if name == "delm_publish" {
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
            "delm_task_update" => task_update(&tx, worker, args)?,
            "delm_task_release" => task_release(&tx, worker, args)?,
            "delm_task_split" => task_split(&tx, worker, args)?,
            "delm_task_finish" => task_finish(&tx, worker, args)?,
            "delm_publish" => publish(
                &tx,
                worker,
                captured.context("missing captured publication")?,
            )?,
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
                let captured =
                    self.workers[worker - 1].freeze(&path, &self.objects, &object, &expected)?;
                (Some(captured), Some(object))
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
        let targets = ["publication_id", "task_id", "finding_id", "check_id"]
            .iter()
            .filter(|key| args.get(**key).is_some())
            .count();
        ensure!(
            targets == 1,
            "expand exactly one publication_id, task_id, finding_id, or check_id"
        );
        if args.get("check_id").is_some() {
            let id = positive_id(args, "check_id")?;
            let encoded: String = self
                .db
                .query_row("SELECT body FROM check_receipts WHERE id=?", [id], |r| {
                    r.get(0)
                })
                .optional()?
                .context("unknown check receipt")?;
            return Ok(serde_json::from_str(&encoded)?);
        }
        if args.get("task_id").is_some() {
            let id = positive_id(args, "task_id")?;
            let (body, owner, state, version): (String, Option<usize>, String, i64) = self
                .db
                .query_row(
                    "SELECT body,owner,state,updated FROM tasks WHERE id=?",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?
                .context("unknown task")?;
            let mut body: Value = serde_json::from_str(&body)?;
            body["task_id"] = json!(id);
            body["task_number"] = json!(self.db.query_row(
                "SELECT COUNT(*) FROM tasks WHERE id<=?",
                [id],
                |r| r.get::<_, u64>(0)
            )?);
            body["owner"] = json!(owner);
            body["state"] = json!(state);
            body["version"] = json!(version);
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

fn task_body(db: &Connection, args: &Value) -> Result<Value> {
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("implementation");
    ensure!(
        ["implementation", "verification", "integration"].contains(&kind),
        "invalid task kind"
    );
    Ok(
        json!({"title": string(args, "title", 256)?, "description": string(args, "description", 2048)?,
        "kind": kind, "interface": optional_string(args, "interface", 2048)?, "dependencies": dependencies(db, args)?,
        "earliest_contribution": optional_string(args, "earliest_contribution", 1024)?,
        "done_when": optional_string(args, "done_when", 1024)?}),
    )
}

fn task_create(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let body = task_body(tx, args)?;
    let id = event(tx, worker, "task_create", &body)?;
    tx.execute(
        "INSERT INTO tasks VALUES (?,?,NULL,'available',?,?)",
        params![id, worker, serde_json::to_string(&body)?, id],
    )?;
    Ok(json!({"task_id": id, "state": "available", "version": id}))
}

fn task_claim(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let id = positive_id(args, "task_id")?;
    let (owner, state, version, body): (Option<usize>, String, i64, String) = tx
        .query_row(
            "SELECT owner,state,updated,body FROM tasks WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .context("unknown task")?;
    ensure!(
        state == "available" || (state == "claimed" && owner == Some(worker)),
        "task is already owned by another worker or finished"
    );
    if state == "claimed" {
        return Ok(json!({"task_id":id,"owner":worker,"state":"claimed","version":version}));
    }
    let body: Value = serde_json::from_str(&body)?;
    if body["kind"] == "integration" {
        let integration: Option<i64> = tx.query_row(
            "SELECT id FROM tasks WHERE state='claimed' AND json_extract(body,'$.kind')='integration' LIMIT 1",
            [], |r| r.get(0)).optional()?;
        ensure!(
            integration.is_none(),
            "integration is already claimed as task {}; contribute other useful work",
            integration.unwrap_or_default()
        );
    }
    let mut result = json!({"task_id": id, "owner": worker, "state": "claimed"});
    let seq = event(tx, worker, "task_claim", &result)?;
    tx.execute(
        "UPDATE tasks SET owner=?,state='claimed',updated=? WHERE id=?",
        params![worker, seq, id],
    )?;
    result["version"] = json!(seq);
    Ok(result)
}

/// Version fencing prevents a delayed request from mutating a released/reclaimed task,
/// including when the same worker claims it again later.
fn owned_task(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<(i64, Value)> {
    let id = positive_id(args, "task_id")?;
    let expected = positive_id(args, "expected_version")?;
    let (owner, state, version, body): (Option<usize>, String, i64, String) = tx
        .query_row(
            "SELECT owner,state,updated,body FROM tasks WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .context("unknown task")?;
    ensure!(
        owner == Some(worker) && state == "claimed",
        "only the current owner may change a claimed task"
    );
    ensure!(
        version == expected,
        "stale task version; read the current board before changing this task"
    );
    Ok((id, serde_json::from_str(&body)?))
}

fn task_update(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let (id, mut body) = owned_task(tx, worker, args)?;
    let mut changed = false;
    for (field, max) in [
        ("title", 256),
        ("description", 2048),
        ("interface", 2048),
        ("earliest_contribution", 1024),
        ("done_when", 1024),
    ] {
        if args.get(field).is_some() {
            body[field] = json!(if ["title", "description"].contains(&field) {
                string(args, field, max)?.to_owned()
            } else {
                optional_string(args, field, max)?
            });
            changed = true;
        }
    }
    if args.get("dependencies").is_some() {
        body["dependencies"] = json!(dependencies(tx, args)?);
        changed = true;
    }
    ensure!(changed, "task update must change at least one task field");
    let seq = event(
        tx,
        worker,
        "task_update",
        &json!({"task_id":id,"task":body}),
    )?;
    tx.execute(
        "UPDATE tasks SET body=?,updated=? WHERE id=?",
        params![serde_json::to_string(&body)?, seq, id],
    )?;
    Ok(json!({"task_id":id,"owner":worker,"state":"claimed","version":seq}))
}

fn task_release(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let (id, mut body) = owned_task(tx, worker, args)?;
    body["handoff"] = json!(string(args, "summary", 2048)?);
    if let Some(value) = args.get("publication_id") {
        let publication = positive_id(&json!({"publication_id":value}), "publication_id")?;
        let author: usize = tx
            .query_row(
                "SELECT worker FROM publications WHERE id=?",
                [publication],
                |r| r.get(0),
            )
            .optional()?
            .context("unknown checkpoint publication")?;
        ensure!(
            author == worker,
            "release checkpoint must be published by the releasing worker"
        );
        body["checkpoint"] = json!(publication);
    }
    let seq = event(
        tx,
        worker,
        "task_release",
        &json!({"task_id":id,"handoff":body["handoff"],"checkpoint":body["checkpoint"]}),
    )?;
    tx.execute(
        "UPDATE tasks SET owner=NULL,state='available',body=?,updated=? WHERE id=?",
        params![serde_json::to_string(&body)?, seq, id],
    )?;
    Ok(json!({"task_id":id,"state":"available","version":seq}))
}

fn task_split(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let (id, mut body) = owned_task(tx, worker, args)?;
    let children = args
        .get("tasks")
        .and_then(Value::as_array)
        .context("tasks must be an array")?;
    ensure!(
        (1..=16).contains(&children.len()),
        "split must expose one to sixteen independent remaining tasks"
    );
    let remaining = args
        .get("remaining")
        .context("remaining describes the work you still own")?;
    let remaining_body = task_body(tx, remaining)?;
    ensure!(
        remaining_body["kind"] == body["kind"],
        "split cannot change the current task kind"
    );
    body = remaining_body;
    let mut created = Vec::new();
    for child in children {
        ensure!(child.is_object(), "each split task must be an object");
        let task = task_create(tx, worker, child)?;
        created.push(task);
    }
    let seq = event(
        tx,
        worker,
        "task_split",
        &json!({"task_id":id,"task":body,"created":created}),
    )?;
    tx.execute(
        "UPDATE tasks SET body=?,updated=? WHERE id=?",
        params![serde_json::to_string(&body)?, seq, id],
    )?;
    Ok(json!({"task_id":id,"owner":worker,"state":"claimed","version":seq,"created":created}))
}

fn task_finish(tx: &Transaction<'_>, worker: usize, args: &Value) -> Result<Value> {
    let (id, _) = owned_task(tx, worker, args)?;
    let mut result = json!({"task_id": id, "state": "done", "summary": string(args, "summary", 2048)?, "whole_task_complete": false});
    let seq = event(tx, worker, "task_finish", &result)?;
    tx.execute(
        "UPDATE tasks SET state='done',updated=? WHERE id=?",
        params![seq, id],
    )?;
    result["version"] = json!(seq);
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
    let mut body = json!({"outcome": outcome, "summary": string(args, "summary", 4096)?, "checks": checks(args)?, "shared_checks": args.get("shared_checks").cloned().unwrap_or_else(||json!([])), "dependency": dependency,
        "declaration_only": true,"expected_revision":revision});
    // Omission means accounting was not supplied; [] explicitly declares that
    // source changes alone satisfy the request. Delivery preserves this fact.
    if args.get("artifacts").is_some() {
        body["artifacts"] = json!(paths(args, "artifacts", false)?);
    }
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
        "SELECT id,author,owner,state,body,updated,task_number FROM (SELECT tasks.*,ROW_NUMBER() OVER(ORDER BY id) AS task_number FROM tasks) ORDER BY (state='done'),updated DESC LIMIT ?",
    )?;
    let tasks = tasks_query.query_map([VIEW_LIMIT], |r| Ok((r.get::<_, i64>(0)?,r.get::<_, usize>(1)?,r.get::<_, Option<usize>>(2)?,r.get::<_, String>(3)?,r.get::<_, String>(4)?,r.get::<_, i64>(5)?,r.get::<_,u64>(6)?)))?
        .map(|row| -> Result<Value> { let (id,author,owner,state,body,version,task_number) = row?; let body: Value=serde_json::from_str(&body)?; Ok(json!({"task_id":id,"task_number":task_number,"author":author,"owner":owner,"state":state,"version":version,"kind":body.get("kind").and_then(Value::as_str).unwrap_or("implementation"),"handoff":body["handoff"],"checkpoint":body["checkpoint"],"title":body["title"],"description":compact(&body["description"],256),"interface":compact(&body["interface"],256),"dependencies":body["dependencies"]})) }).collect::<Result<Vec<_>>>()?;
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
    let mut checks_query =
        db.prepare("SELECT id,worker,revision,body FROM check_receipts ORDER BY id DESC LIMIT ?")?;
    let check_receipts = checks_query.query_map([VIEW_LIMIT], |r|Ok((r.get::<_,i64>(0)?,r.get::<_,usize>(1)?,r.get::<_,u64>(2)?,r.get::<_,String>(3)?)))?
        .map(|row| -> Result<Value> { let(id,worker,revision,body)=row?; let body:Value=serde_json::from_str(&body)?;
            Ok(json!({"receipt_id":id,"worker":worker,"request_revision":revision,"summary":body["summary"],"passed":body["passed"],"inputs_unchanged":body["inputs_unchanged"],"reusable":body["reusable"],"command":body["native"]["command"]})) })
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"sequence":sequence,"request_revision":revision,"workers":workers,"tasks":tasks,"publications":publications,"findings":findings,"check_receipts":check_receipts,"view_limit":VIEW_LIMIT,
            "collections":discovery::counts(db)?}),
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
        ensure!(
            exists,
            "unknown dependency publication {id}; use publication_id values returned by delm_publish or delm_list(collection=publications), not task IDs or task numbers; omit dependencies until a contribution exists"
        );
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

/// DynamicToolSpec values sent to the installed native app-server.
pub fn tool_definitions() -> Vec<Value> {
    let text = json!({"type":"string"});
    let bounded_text = |limit: usize, required: bool| {
        json!({"type":"string","minLength":usize::from(required),"maxLength":limit,
        "description":format!("At most {limit} UTF-8 bytes. Keep this field concise; publish detailed content as a file.")})
    };
    let title = bounded_text(256, true);
    let description = bounded_text(2048, true);
    let interface = bounded_text(2048, false);
    let task_note = bounded_text(1024, false);
    let publication_ids = json!({"type":"array","maxItems":128,"items":{"type":"integer","minimum":1},
        "description":"Existing publication_id integers returned by delm_publish or delm_list(collection=publications). These are shared code contributions, not task IDs, task numbers, or future prerequisites. Omit or use [] until a contribution exists."});
    let strings = json!({"type":"array","items":{"type":"string"}});
    let ids = json!({"type":"array","items":{"type":"integer","minimum":1}});
    let mut definitions = Vec::new();
    let mut define = |name: &str, description: &str, properties: Value, required: &[&str]| {
        definitions.push(json!({"type":"function","name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"deferLoading":false}));
    };
    define(
        "delm_status",
        "Read compact board, including collection totals. Use delm_list only when you need records beyond the compact view. Optionally update your status or append a useful finding; updates require a unique idempotency_key. Waiting names a concrete dependency.",
        json!({"idempotency_key":text,"state":{"enum":["working","waiting","blocked"]},"summary":text,"dependency":text,"finding":text}),
        &[],
    );
    define(
        "delm_list",
        "Discover a bounded page of tasks, publications, findings, or check receipts, ordered by stable ID. Use the returned cursor to continue. Optional task_state and owner filters find current work; restart without a cursor if task ownership changed. This is read-only; it does not claim work or append context. Use delm_expand for a selected record's full details.",
        json!({"collection":{"enum":["tasks","publications","findings","checks"]},"limit":{"type":"integer","minimum":1,"maximum":24},"cursor":{"type":"string","maxLength":1024},"task_state":{"enum":["available","claimed","done"]},"owner":{"enum":["self","peer"]}}),
        &["collection"],
    );
    define(
        "delm_task_create",
        "Expose a useful independent contribution. UTF-8 byte limits: title 256; description/interface 2048 each; earliest_contribution/done_when 1024 each. dependencies must be existing publication_id values from delm_publish or delm_list(collection=publications), never task numbers. Use integration for temporary ownership of assembling the shared result, or verification for a scoped check; one integration task may be claimed at a time.",
        json!({"idempotency_key":text,"title":title,"description":description,"kind":{"enum":["implementation","verification","integration"]},"interface":interface,"dependencies":publication_ids,"earliest_contribution":task_note,"done_when":task_note}),
        &["idempotency_key", "title", "description"],
    );
    define(
        "delm_task_claim",
        "Atomically claim ready work. The returned version fences updates, release, split and finish against stale ownership.",
        json!({"idempotency_key":text,"task_id":{"type":"integer","minimum":1}}),
        &["idempotency_key", "task_id"],
    );
    define(
        "delm_task_finish",
        "Finish your claimed contribution using its current version. Take useful remaining work; only the current integrator assembles the shared result.",
        json!({"idempotency_key":text,"task_id":{"type":"integer","minimum":1},"expected_version":{"type":"integer","minimum":1},"summary":text}),
        &["idempotency_key", "task_id", "expected_version", "summary"],
    );
    define(
        "delm_task_update",
        "Update the boundary, interface or remaining work of your claimed task using its current board version. UTF-8 byte limits: title 256; description/interface 2048 each; earliest_contribution/done_when 1024 each. dependencies must be existing publication_id values from delm_publish or delm_list(collection=publications), never task numbers.",
        json!({"idempotency_key":text,"task_id":{"type":"integer","minimum":1},"expected_version":{"type":"integer","minimum":1},"title":title,"description":description,"interface":interface,"dependencies":publication_ids,"earliest_contribution":task_note,"done_when":task_note}),
        &["idempotency_key", "task_id", "expected_version"],
    );
    define(
        "delm_task_release",
        "Release your claim with a concise handoff. Publish partially implemented files first and name their publication_id; the peer may then claim and continue.",
        json!({"idempotency_key":text,"task_id":{"type":"integer","minimum":1},"expected_version":{"type":"integer","minimum":1},"summary":text,"publication_id":{"type":"integer","minimum":1}}),
        &["idempotency_key", "task_id", "expected_version", "summary"],
    );
    let task_schema = json!({"type":"object","properties":{"title":title,"description":description,"kind":{"enum":["implementation","verification","integration"]},"interface":interface,"dependencies":publication_ids,"earliest_contribution":task_note,"done_when":task_note},"required":["title","description"],"additionalProperties":false});
    define(
        "delm_task_split",
        "Keep a smaller remaining portion and atomically expose independent work to your peer. remaining and tasks use the task-create fields; remaining keeps the current task kind.",
        json!({"idempotency_key":text,"task_id":{"type":"integer","minimum":1},"expected_version":{"type":"integer","minimum":1},"remaining":task_schema,"tasks":{"type":"array","minItems":1,"maxItems":16,"items":task_schema}}),
        &[
            "idempotency_key",
            "task_id",
            "expected_version",
            "remaining",
            "tasks",
        ],
    );
    define(
        "delm_check_begin",
        "Capture scoped input file versions BEFORE running a native check you intend to share. Include source, test, configuration and lockfiles relevant to that command. Explicit files only; no directories. This scope does not attest external state or undeclared inputs.",
        json!({"idempotency_key":text,"summary":text,"paths":strings}),
        &["idempotency_key", "summary", "paths"],
    );
    define(
        "delm_check_finish",
        "Bind a begun snapshot to your observed completed native command. Returns a reusable receipt only when the command passed and scoped inputs stayed unchanged. Failed checks remain visible.",
        json!({"idempotency_key":text,"snapshot_id":{"type":"integer","minimum":1},"command_id":text}),
        &["idempotency_key", "snapshot_id", "command_id"],
    );
    define(
        "delm_publish",
        "Freeze selected project-relative files or deletions as an immutable contribution. Publish useful partial work early; paths may be empty for an interface or finding. Large artifacts use large_artifact=true (4 GiB limit, otherwise 64 MiB).",
        json!({"idempotency_key":text,"summary":text,"paths":strings,"dependencies":publication_ids,"interfaces":text,"unfinished":text,"checks":{"type":"array","items":{}},"large_artifact":{"type":"boolean"}}),
        &["idempotency_key", "summary", "paths"],
    );
    define(
        "delm_expand",
        "Inspect exactly one publication_id, task_id, finding_id, or check_id. Select publication paths and use offset/max_bytes for large text. Binary assets are imported losslessly with delm_apply.",
        json!({"publication_id":{"type":"integer","minimum":1},"task_id":{"type":"integer","minimum":1},"finding_id":{"type":"integer","minimum":1},"check_id":{"type":"integer","minimum":1},"paths":strings,"offset":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":1,"maximum":32768}}),
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
        "Declare the one assembled result outcome with current board request_revision as expected_revision. artifacts lists every requested generated output file or directory, including Git-ignored outputs, relative to your private project; exclude dependency, cache and credential paths. Use [] explicitly for source-only work. Omitting artifacts leaves output accounting unconfirmed. checks are your native command IDs; shared_checks are validated receipt IDs from either peer whose scoped inputs still match. Then end your turn. Complete means the full result is ready. Waiting requires dependency task:<id> or worker:<other worker>; it cannot name your own work.",
        json!({"idempotency_key":text,"expected_revision":{"type":"integer","minimum":1},"outcome":{"enum":["complete","partial","blocked","waiting"]},"summary":text,"checks":{"type":"array","items":{}},"shared_checks":ids,"artifacts":strings,"dependency":{"type":"string","description":"For waiting, task:<positive task ID> or worker:<1|2>; use the other worker, not yourself."}}),
        &["idempotency_key", "expected_revision", "outcome", "summary"],
    );
    definitions
}

#[cfg(test)]
mod tests;
