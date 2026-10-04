//! Explicitly scoped evidence reuse. A native command must start after the
//! snapshot fence; neither publication text nor model-supplied results attest it.
use super::*;
use std::collections::{BTreeMap, HashMap, HashSet};

impl Board {
    /// Shared services use the same confined, explicit-input fingerprinting as
    /// check receipts. Callers choose the scope; this is not a whole-tree claim.
    pub fn input_snapshot(&self, worker: usize, scope: &[String]) -> Result<Value> {
        ensure!((1..=2).contains(&worker), "unbound worker identity");
        ensure!(
            !scope.is_empty() && scope.len() <= 2048,
            "input scope needs 1 to 2048 explicit file paths"
        );
        ensure!(
            scope.iter().collect::<HashSet<_>>().len() == scope.len(),
            "duplicate input scope path"
        );
        Ok(serde_json::to_value(self.check_versions(worker, scope)?)?)
    }

    pub fn worker_path(&self, worker: usize) -> Result<&Path> {
        ensure!((1..=2).contains(&worker), "unbound worker identity");
        self.workers[worker - 1].verify()?;
        Ok(&self.workers[worker - 1].path)
    }

    /// Capture selected input files before a check. Sample the native transport
    /// sequence after capture so queued/previous command starts cannot qualify.
    pub fn begin_check(
        &mut self,
        worker: usize,
        args: Value,
        boundary: impl FnOnce() -> u64,
    ) -> Result<Value> {
        ensure!((1..=2).contains(&worker), "unbound worker identity");
        self.lock.lock_exclusive()?;
        let result = (|| {
            if let Some(replay) = replay(&self.db, worker, "delm_check_begin", &args)? {
                return Ok(replay);
            }
            let scope = paths(&args, "paths", false)?;
            ensure!(
                !scope.is_empty() && scope.len() <= 2048,
                "check scope needs 1 to 2048 explicit input file paths"
            );
            let versions = self.check_versions(worker, &scope)?;
            let fence = boundary();
            let tx = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let revision: u64 =
                tx.query_row("SELECT revision FROM context WHERE id=1", [], |r| r.get(0))?;
            let body = json!({"summary":string(&args,"summary",2048)?,"files":versions,
                "scope":"explicit_input_files","fence":fence});
            let id = event(
                &tx,
                worker,
                "check_begin",
                &json!({"summary":body["summary"],"file_count":scope.len()}),
            )?;
            tx.execute(
                "INSERT INTO check_snapshots VALUES (?,?,?,?)",
                params![id, worker, revision, serde_json::to_string(&body)?],
            )?;
            let response = json!({"result":{"snapshot_id":id,"request_revision":revision,"file_count":scope.len()},"board":view(&tx)?});
            remember(&tx, worker, "delm_check_begin", &args, &response)?;
            tx.commit()?;
            Ok(response)
        })();
        let unlock = FileExt::unlock(&self.lock);
        if result.is_ok() {
            unlock?;
        }
        result
    }

    /// `native_checks` comes from the runtime's bound worker thread, never tool
    /// arguments. Failed and changed-input checks remain visible as receipts.
    pub fn finish_check(
        &mut self,
        worker: usize,
        args: Value,
        native_checks: &HashMap<String, Value>,
    ) -> Result<Value> {
        ensure!((1..=2).contains(&worker), "unbound worker identity");
        self.lock.lock_exclusive()?;
        let result = (|| {
            if let Some(replay) = replay(&self.db, worker, "delm_check_finish", &args)? {
                return Ok(replay);
            }
            let snapshot = positive_id(&args, "snapshot_id")?;
            let (owner, revision, encoded): (usize, u64, String) = self
                .db
                .query_row(
                    "SELECT worker,revision,body FROM check_snapshots WHERE id=?",
                    [snapshot],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?
                .context("unknown check snapshot")?;
            ensure!(
                owner == worker,
                "only the worker that captured this scope may finish its check"
            );
            let current: u64 =
                self.db
                    .query_row("SELECT revision FROM context WHERE id=1", [], |r| r.get(0))?;
            ensure!(
                revision == current,
                "check snapshot belongs to an obsolete request revision"
            );
            let before: Value = serde_json::from_str(&encoded)?;
            let id = string(&args, "command_id", 256)?;
            let native = native_checks
                .get(id)
                .context("command was not observed from this native worker")?;
            ensure!(
                native["type"] == "commandExecution" && native["id"].as_str() == Some(id),
                "invalid native command record"
            );
            ensure!(
                native["_delm_revision"].as_u64() == Some(revision),
                "command belongs to an obsolete or unbound request revision"
            );
            let started = native["_delm_started_sequence"]
                .as_u64()
                .context("native command has no observed start boundary")?;
            ensure!(
                started > before["fence"].as_u64().context("invalid check fence")?,
                "command started before the check input snapshot; capture scope before running it"
            );
            ensure!(
                matches!(native["status"].as_str(), Some("completed" | "failed")),
                "native command has not finished"
            );
            let exit = native["exitCode"]
                .as_i64()
                .context("native command has no exit status")?;
            ensure!(
                native["command"].as_str().is_some_and(|s| !s.is_empty()),
                "native command omitted its command text"
            );
            let cwd = native["cwd"]
                .as_str()
                .context("native command omitted its cwd")?;
            let cwd = fs::canonicalize(cwd).context("check execution directory is unavailable")?;
            ensure!(
                cwd.starts_with(&self.workers[worker - 1].path),
                "shared check must execute inside its worker project"
            );
            let scope = before["files"]
                .as_object()
                .context("invalid check scope")?
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            let after = self.check_versions(worker, &scope)?;
            let unchanged = serde_json::to_value(&after)? == before["files"];
            let passed = exit == 0 && native["status"] == "completed";
            // The run journal retains native output. Coordination needs the
            // observed command identity and outcome, not another full log copy.
            let native = [
                "type",
                "id",
                "command",
                "cwd",
                "status",
                "exitCode",
                "_delm_revision",
                "_delm_started_sequence",
            ]
            .into_iter()
            .map(|key| (key.to_owned(), native[key].clone()))
            .collect::<serde_json::Map<_, _>>();
            let mut receipt = json!({"snapshot_id":snapshot,"worker":worker,"request_revision":revision,
                "summary":before["summary"],"scope":"explicit_input_files","files":before["files"],
                "inputs_unchanged":unchanged,"passed":passed,"reusable":unchanged&&passed,"native":native});
            let tx = self
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let receipt_id = event(
                &tx,
                worker,
                "check_finish",
                &json!({"snapshot_id":snapshot,"command_id":id,"passed":passed,"inputs_unchanged":unchanged}),
            )?;
            receipt["receipt_id"] = json!(receipt_id);
            tx.execute(
                "INSERT INTO check_receipts VALUES (?,?,?,?)",
                params![
                    receipt_id,
                    worker,
                    revision,
                    serde_json::to_string(&receipt)?
                ],
            )?;
            let response = json!({"result":receipt,"board":view(&tx)?});
            remember(&tx, worker, "delm_check_finish", &args, &response)?;
            tx.commit()?;
            Ok(response)
        })();
        let unlock = FileExt::unlock(&self.lock);
        if result.is_ok() {
            unlock?;
        }
        result
    }

    fn check_versions(
        &self,
        worker: usize,
        scope: &[String],
    ) -> Result<BTreeMap<String, Option<FileVersion>>> {
        scope.iter().map(|path| {
            self.require_access(worker, path, false)?;
            Ok((path.clone(), self.workers[worker-1].version(path)
                .with_context(||format!("snapshot explicit input file {path}; directories must be expanded to their input files"))?))
        }).collect()
    }

    /// Validate an imported receipt against this worker's current scoped files.
    /// This proves scope equality and observed execution, not complete coverage,
    /// ambient environment equality, or absence of transient writes during a check.
    pub fn shared_checks(&self, worker: usize, args: &Value, revision: u64) -> Result<Vec<Value>> {
        ensure!((1..=2).contains(&worker), "unbound worker identity");
        let Some(requested) = args.get("shared_checks") else {
            return Ok(Vec::new());
        };
        let ids = requested
            .as_array()
            .context("shared_checks must be receipt IDs")?;
        ensure!(ids.len() <= 64, "too many shared check receipts");
        let mut seen = HashSet::new();
        ids.iter().map(|value| {
            let id = value.as_i64().filter(|id|*id>0).context("invalid shared check receipt ID")?;
            ensure!(seen.insert(id), "duplicate shared check receipt");
            let encoded: String = self.db.query_row("SELECT body FROM check_receipts WHERE id=?", [id], |r|r.get(0))
                .optional()?.context("unknown shared check receipt")?;
            let receipt: Value = serde_json::from_str(&encoded)?;
            ensure!(receipt["request_revision"].as_u64() == Some(revision), "shared check is for an obsolete request revision");
            ensure!(receipt["reusable"] == true, "shared check failed or its inputs changed during execution");
            let scope = receipt["files"].as_object().context("invalid check receipt scope")?.keys().cloned().collect::<Vec<_>>();
            ensure!(serde_json::to_value(self.check_versions(worker,&scope)?)? == receipt["files"],
                "shared check scope differs from your current files; import its contribution or check your changed inputs");
            Ok(receipt)
        }).collect()
    }
}

fn replay(db: &Connection, worker: usize, name: &str, args: &Value) -> Result<Option<Value>> {
    ensure!(args.is_object(), "tool arguments must be an object");
    ensure!(
        serde_json::to_vec(args)?.len() <= MAX_ARGUMENT_BYTES,
        "check metadata exceeds limit"
    );
    let key = string(args, "idempotency_key", 128)?;
    let prior: Option<(String, String, String)> = db
        .query_row(
            "SELECT name,digest,response FROM requests WHERE worker=? AND key=? AND state='done'",
            params![worker, key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((old_name, digest, response)) = prior {
        ensure!(
            old_name == name
                && digest == format!("{:x}", Sha256::digest(serde_json::to_vec(args)?)),
            "idempotency key was already used with different arguments"
        );
        return Ok(Some(serde_json::from_str(&response)?));
    }
    Ok(None)
}

fn remember(
    tx: &Transaction<'_>,
    worker: usize,
    name: &str,
    args: &Value,
    response: &Value,
) -> Result<()> {
    tx.execute(
        "INSERT INTO requests(worker,key,name,digest,state,response) VALUES (?,?,?,?,'done',?)",
        params![
            worker,
            string(args, "idempotency_key", 128)?,
            name,
            format!("{:x}", Sha256::digest(serde_json::to_vec(args)?)),
            serde_json::to_string(response)?
        ],
    )?;
    Ok(())
}
