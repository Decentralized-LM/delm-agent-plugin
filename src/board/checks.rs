//! Explicitly scoped evidence reuse. A native command must start after the
//! snapshot fence; neither publication text nor model-supplied results attest it.
use super::*;
use crate::evidence::CommandEvidence;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Authoritative scoped receipts for one worker result. Keep the original Board
/// alive through verification so its bound directory handles retain identity.
/// This evidence cannot be restored from model input or a saved JSON document.
pub(crate) struct CompletionChecks {
    worker: usize,
    root: PathBuf,
    receipts: Vec<Value>,
    inputs: BTreeMap<String, Option<FileVersion>>,
}

impl CompletionChecks {
    pub(crate) fn receipts(&self) -> &[Value] {
        &self.receipts
    }

    /// Recheck only the union of explicitly named inputs through the original
    /// board's confined readers and native access policy, including absent files.
    pub(crate) fn verify<'a>(&self, board: &'a Board) -> Result<&'a Path> {
        let root = board.worker_path(self.worker)?;
        ensure!(
            root == self.root,
            "completion checks belong to another worker root"
        );
        let scope = self.inputs.keys().cloned().collect::<Vec<_>>();
        let actual = board
            .check_versions(self.worker, &scope)
            .context("shared check input verification failed")?;
        for (path, expected) in &self.inputs {
            ensure!(
                actual.get(path) == Some(expected),
                "shared check input changed at {path}"
            );
        }
        board.worker_path(self.worker)
    }
}

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
        self.finish_check_record(worker, args, |id| {
            let native = native_checks
                .get(id)
                .context("command was not observed from this native worker")?;
            let evidence = CommandEvidence::from_codex(native)?;
            // Full native output remains in the run journal.
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
            Ok((evidence, Value::Object(native)))
        })
    }

    pub fn finish_check_with_evidence(
        &mut self,
        worker: usize,
        args: Value,
        native_checks: &HashMap<String, CommandEvidence>,
    ) -> Result<Value> {
        self.finish_check_record(worker, args, |id| {
            let evidence = native_checks
                .get(id)
                .context("command was not observed from this native worker")?;
            Ok((evidence.clone(), serde_json::to_value(evidence)?))
        })
    }

    fn finish_check_record(
        &mut self,
        worker: usize,
        args: Value,
        native_record: impl FnOnce(&str) -> Result<(CommandEvidence, Value)>,
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
            let (evidence, native) = native_record(id)?;
            let passed = evidence.validate_for(id, revision)?;
            let started = evidence
                .started_sequence
                .context("native command has no observed start boundary")?;
            ensure!(
                started > before["fence"].as_u64().context("invalid check fence")?,
                "command started before the check input snapshot; capture scope before running it"
            );
            let cwd = fs::canonicalize(&evidence.cwd)
                .context("check execution directory is unavailable")?;
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
            let mut receipt = json!({"snapshot_id":snapshot,"worker":worker,"request_revision":revision,
                "summary":before["summary"],"scope":"explicit_input_files","files":before["files"],
                "inputs_unchanged":unchanged,"passed":passed,"reusable":unchanged&&passed,"native":native,"evidence":evidence});
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
            let receipt = self.shared_receipt(id, revision)?;
            let scope = receipt["files"].as_object().context("invalid check receipt scope")?.keys().cloned().collect::<Vec<_>>();
            ensure!(serde_json::to_value(self.check_versions(worker,&scope)?)? == receipt["files"],
                "shared check scope differs from your current files; import its contribution or check your changed inputs");
            Ok(receipt)
        }).collect()
    }

    fn shared_receipt(&self, id: i64, revision: u64) -> Result<Value> {
        let encoded: String = self
            .db
            .query_row("SELECT body FROM check_receipts WHERE id=?", [id], |row| {
                row.get(0)
            })
            .optional()?
            .context("unknown shared check receipt")?;
        let receipt: Value = serde_json::from_str(&encoded)?;
        ensure!(
            receipt["request_revision"].as_u64() == Some(revision),
            "shared check is for an obsolete request revision"
        );
        ensure!(
            receipt["reusable"] == true,
            "shared check failed or its inputs changed during execution"
        );
        Ok(receipt)
    }

    /// Bind completion evidence to this worker without conflating verification
    /// inputs with the source and artifacts selected for delivery.
    pub(crate) fn completion_checks(
        &self,
        worker: usize,
        args: &Value,
        revision: u64,
    ) -> Result<CompletionChecks> {
        let root = self.worker_path(worker)?.to_path_buf();
        let ids = match args.get("shared_checks") {
            None => &[][..],
            Some(value) => value
                .as_array()
                .context("shared_checks must be receipt IDs")?
                .as_slice(),
        };
        ensure!(ids.len() <= 64, "too many shared check receipts");
        let mut seen = HashSet::new();
        let mut receipts = Vec::with_capacity(ids.len());
        let mut inputs = BTreeMap::new();
        for value in ids {
            let id = value
                .as_i64()
                .filter(|id| *id > 0)
                .context("invalid shared check receipt ID")?;
            ensure!(seen.insert(id), "duplicate shared check receipt");
            let receipt = self.shared_receipt(id, revision)?;
            let scope: BTreeMap<String, Option<FileVersion>> =
                serde_json::from_value(receipt["files"].clone())
                    .context("invalid check receipt scope")?;
            ensure!(
                !scope.is_empty() && scope.len() <= 2048,
                "check scope needs 1 to 2048 explicit input file paths"
            );
            for (path, version) in scope {
                if let Some(prior) = inputs.get(&path) {
                    ensure!(
                        prior == &version,
                        "shared checks require conflicting input versions at {path}"
                    );
                } else {
                    inputs.insert(path, version);
                }
            }
            receipts.push(receipt);
        }
        let checks = CompletionChecks {
            worker,
            root,
            receipts,
            inputs,
        };
        checks.verify(self)?;
        Ok(checks)
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

#[cfg(test)]
mod completion_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture() -> (tempfile::TempDir, Board) {
        let temp = tempfile::tempdir().unwrap();
        let run = temp.path().join("run");
        let baseline = temp.path().join("baseline");
        let workers = [temp.path().join("worker-1"), temp.path().join("worker-2")];
        for path in [&run, &baseline, &workers[0], &workers[1]] {
            fs::create_dir(path).unwrap();
        }
        let mut board = Board::open(&run, &baseline, workers).unwrap();
        for worker in 1..=2 {
            let root = board.worker_path(worker).unwrap().to_path_buf();
            board
                .set_worker_scopes(
                    worker,
                    vec![FilesystemScope {
                        path: root,
                        access: FilesystemAccess::Write,
                    }],
                )
                .unwrap();
        }
        (temp, board)
    }

    fn receipt(board: &mut Board, worker: usize, key: &str, paths: &[&str]) -> i64 {
        let root = board.worker_path(worker).unwrap().to_path_buf();
        let begun = board.begin_check(worker, json!({
            "idempotency_key":format!("begin-{key}"), "summary":"Focused check", "paths":paths,
        }), || 10).unwrap();
        let native = HashMap::from([(
            "test-command".into(),
            json!({
                "type":"commandExecution", "id":"test-command", "command":"node --test",
                "cwd":root, "status":"completed", "exitCode":0,
                "_delm_revision":1, "_delm_started_sequence":11,
            }),
        )]);
        board
            .finish_check(
                worker,
                json!({
                    "idempotency_key":format!("finish-{key}"),
                    "snapshot_id":begun["result"]["snapshot_id"], "command_id":"test-command",
                }),
                &native,
            )
            .unwrap()["result"]["receipt_id"]
            .as_i64()
            .unwrap()
    }

    #[test]
    fn completion_checks_reuse_peer_receipts_and_union_only_explicit_inputs() {
        let (_temp, mut board) = fixture();
        for worker in 1..=2 {
            let root = board.worker_path(worker).unwrap();
            fs::create_dir_all(root.join("node_modules/library")).unwrap();
            fs::write(root.join("node_modules/library/index.js"), b"dependency").unwrap();
        }
        let peer = receipt(
            &mut board,
            1,
            "peer",
            &["node_modules/library/index.js", "optional.json"],
        );
        let local = receipt(&mut board, 2, "local", &["node_modules/library/index.js"]);
        let declaration = json!({"shared_checks":[peer, local]});
        let checks = board.completion_checks(2, &declaration, 1).unwrap();
        assert_eq!(checks.receipts().len(), 2);
        assert_eq!(checks.receipts()[0]["worker"], 1);
        assert_eq!(checks.inputs.len(), 2);
        let root = board.worker_path(2).unwrap().to_path_buf();
        fs::write(root.join("node_modules/library/unrelated.js"), b"unrelated").unwrap();
        checks.verify(&board).unwrap();
        fs::write(root.join("optional.json"), b"new input").unwrap();
        assert!(checks.verify(&board).is_err());
        fs::remove_file(root.join("optional.json")).unwrap();
        checks.verify(&board).unwrap();
        fs::write(root.join("node_modules/library/index.js"), b"different").unwrap();
        assert!(checks.verify(&board).is_err());
        assert!(board.completion_checks(2, &declaration, 1).is_err());
    }

    #[test]
    fn completion_checks_reject_conflicting_receipt_expectations() {
        let (_temp, mut board) = fixture();
        fs::write(board.worker_path(1).unwrap().join("input.js"), b"first").unwrap();
        fs::write(board.worker_path(2).unwrap().join("input.js"), b"second").unwrap();
        let first = receipt(&mut board, 1, "first", &["input.js"]);
        let second = receipt(&mut board, 2, "second", &["input.js"]);
        let error = board
            .completion_checks(2, &json!({"shared_checks":[first, second]}), 1)
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("conflicting input versions at input.js"),
            "{error}"
        );
    }

    #[test]
    fn completion_checks_preserve_root_identity_without_receipts() {
        let (_temp, board) = fixture();
        let checks = board.completion_checks(1, &json!({}), 1).unwrap();
        let root = checks.verify(&board).unwrap().to_path_buf();
        fs::rename(&root, root.with_extension("retained")).unwrap();
        fs::create_dir(&root).unwrap();
        assert!(checks.verify(&board).is_err());
        assert!(board.completion_checks(1, &json!({}), 1).is_err());
    }

    #[test]
    fn completion_checks_revalidate_permissions_and_confined_file_identity() {
        let (_temp, mut board) = fixture();
        let root = board.worker_path(1).unwrap().to_path_buf();
        fs::create_dir(root.join("inputs")).unwrap();
        let input = root.join("inputs/check.js");
        fs::write(&input, b"checked").unwrap();
        fs::set_permissions(&input, fs::Permissions::from_mode(0o644)).unwrap();
        let id = receipt(&mut board, 1, "safety", &["inputs/check.js"]);
        let declaration = json!({"shared_checks":[id]});
        let checks = board.completion_checks(1, &declaration, 1).unwrap();

        fs::set_permissions(&input, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(checks.verify(&board).is_err());
        fs::set_permissions(&input, fs::Permissions::from_mode(0o644)).unwrap();
        checks.verify(&board).unwrap();
        fs::remove_file(&input).unwrap();
        assert!(checks.verify(&board).is_err());
        fs::write(&input, b"checked").unwrap();
        fs::set_permissions(&input, fs::Permissions::from_mode(0o644)).unwrap();
        checks.verify(&board).unwrap();

        fs::rename(root.join("inputs"), root.join("retained-inputs")).unwrap();
        symlink(root.join("retained-inputs"), root.join("inputs")).unwrap();
        assert!(checks.verify(&board).is_err());
        fs::remove_file(root.join("inputs")).unwrap();
        fs::rename(root.join("retained-inputs"), root.join("inputs")).unwrap();
        checks.verify(&board).unwrap();

        board.set_worker_scopes(1, vec![]).unwrap();
        assert!(checks.verify(&board).is_err());
        assert!(board.completion_checks(1, &declaration, 1).is_err());
    }

    #[test]
    fn completion_checks_retain_receipt_identity_and_revision_requirements() {
        let (_temp, mut board) = fixture();
        fs::write(board.worker_path(1).unwrap().join("input.js"), b"checked").unwrap();
        let id = receipt(&mut board, 1, "identity", &["input.js"]);
        assert!(
            board
                .completion_checks(1, &json!({"shared_checks":[id]}), 2)
                .is_err()
        );
        assert!(
            board
                .completion_checks(1, &json!({"shared_checks":[id,id]}), 1)
                .is_err()
        );
        assert!(
            board
                .completion_checks(1, &json!({"shared_checks":[0]}), 1)
                .is_err()
        );
        assert!(
            board
                .completion_checks(1, &json!({"shared_checks":[-1]}), 1)
                .is_err()
        );
        assert!(
            board
                .completion_checks(1, &json!({"shared_checks":[99999]}), 1)
                .is_err()
        );
        assert!(
            board
                .completion_checks(1, &json!({"shared_checks":null}), 1)
                .is_err()
        );
        assert!(board.completion_checks(0, &json!({}), 1).is_err());
    }
}
