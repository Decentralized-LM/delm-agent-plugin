//! Runtime evidence for a normally completed native turn. This establishes
//! artifact identity and truthful command references, not semantic correctness.
use crate::board::Board;
use crate::evidence::CommandEvidence;
use crate::workspace::{self, Manifest};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Completion {
    pub revision: u64,
    pub declaration: Value,
    pub checks: Vec<Value>,
    #[serde(default)]
    pub shared_checks: Vec<Value>,
    pub manifest: Manifest,
    #[serde(default)]
    pub accepted: Option<workspace::AcceptedResult>,
    #[serde(default)]
    pub result_policy: workspace::ResultPolicy,
}

impl Completion {
    #[cfg(test)]
    pub fn capture(
        root: &Path,
        declaration: &Value,
        native_checks: &HashMap<String, Value>,
        revision: u64,
        result_policy: &workspace::ResultPolicy,
    ) -> Result<Self> {
        Self::capture_with_shared(
            root,
            declaration,
            native_checks,
            revision,
            result_policy,
            Vec::new(),
            &workspace::manifest(root)?,
        )
    }

    /// Test helper for callers that do not use shared check receipts.
    #[cfg(test)]
    pub fn capture_with_shared(
        root: &Path,
        declaration: &Value,
        native_checks: &HashMap<String, Value>,
        revision: u64,
        result_policy: &workspace::ResultPolicy,
        shared_checks: Vec<Value>,
        baseline: &Manifest,
    ) -> Result<Self> {
        let checks = validate_checks(declaration, native_checks, revision)?;
        ensure!(
            shared_checks.is_empty(),
            "shared checks require a bound board"
        );
        Self::capture_candidate(
            root,
            declaration,
            checks,
            revision,
            result_policy,
            shared_checks,
            baseline,
        )
    }

    pub fn capture_with_evidence(
        board: &Board,
        worker: usize,
        declaration: &Value,
        native_checks: &HashMap<String, CommandEvidence>,
        revision: u64,
        result_policy: &workspace::ResultPolicy,
        baseline: &Manifest,
    ) -> Result<Self> {
        let checks = validate_evidence(declaration, native_checks, revision)?;
        let inputs = board.completion_checks(worker, declaration, revision)?;
        let candidate = Self::capture_candidate(
            board.worker_path(worker)?,
            declaration,
            checks,
            revision,
            result_policy,
            inputs.receipts().to_vec(),
            baseline,
        )?;
        // Input scopes can include environment files intentionally omitted from
        // delivery. Recheck their actual files after selecting the outputs.
        inputs.verify(board)?;
        Ok(candidate)
    }

    /// Re-establish saved receipt provenance and current input identity before
    /// delivery. A serialized candidate alone is not authoritative check evidence.
    pub fn verify_shared_inputs(&self, board: &Board, worker: usize) -> Result<()> {
        ensure!(
            self.declaration["outcome"].as_str() == Some("complete")
                && self.declaration["expected_revision"].as_u64() == Some(self.revision),
            "saved completion declaration does not match its request revision"
        );
        let inputs = board.completion_checks(worker, &self.declaration, self.revision)?;
        ensure!(
            inputs.receipts() == self.shared_checks,
            "saved shared check receipts differ from the authoritative board"
        );
        Ok(())
    }

    /// Capture deliverables and attach already observed command evidence.
    /// The host adapter must also validate receipt inputs: the delivery manifest
    /// intentionally omits newly installed dependencies and other environment files.
    pub(crate) fn capture_candidate(
        root: &Path,
        declaration: &Value,
        checks: Vec<Value>,
        revision: u64,
        result_policy: &workspace::ResultPolicy,
        shared_checks: Vec<Value>,
        baseline: &Manifest,
    ) -> Result<Self> {
        ensure!(
            declaration["outcome"].as_str() == Some("complete"),
            "candidate must declare the whole request complete"
        );
        ensure!(
            declaration["expected_revision"].as_u64() == Some(revision),
            "candidate declaration is for an obsolete request revision"
        );
        let artifacts = match declaration.get("artifacts") {
            None => Vec::new(),
            Some(value) => value
                .as_array()
                .context("artifacts must be an array")?
                .iter()
                .map(|path| {
                    path.as_str()
                        .map(str::to_owned)
                        .context("artifact paths must be strings")
                })
                .collect::<Result<Vec<_>>>()?,
        };
        let mut selection = workspace::ResultSelection::new(baseline, artifacts)?;
        selection.artifacts_declared = declaration.get("artifacts").is_some();
        let accepted = selection.capture(root)?;
        let manifest = accepted.manifest.clone();
        let declared = declaration
            .get("shared_checks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        ensure!(
            declared.len() == shared_checks.len(),
            "shared receipt validation is missing"
        );
        for (id, receipt) in declared.iter().zip(&shared_checks) {
            ensure!(
                id == &receipt["receipt_id"]
                    && receipt["request_revision"].as_u64() == Some(revision)
                    && receipt["reusable"] == true,
                "invalid or obsolete shared check receipt"
            );
        }
        Ok(Self {
            revision,
            declaration: declaration.clone(),
            checks,
            shared_checks,
            manifest,
            accepted: Some(accepted),
            result_policy: result_policy.clone(),
        })
    }

    /// Call after all owned writers have stopped, before retaining or cleaning.
    pub fn verify(&self, root: &Path) -> Result<()> {
        if let Some(accepted) = &self.accepted {
            accepted.verify(root)?;
            return Ok(());
        }
        ensure!(
            workspace::manifest_for_result(root, &self.result_policy)? == self.manifest,
            "candidate files changed between native completion and stopped-writer verification; preserve both workspaces without publishing this result"
        );
        Ok(())
    }

    pub fn deliver(
        &self,
        prepared: &workspace::PreparedWorkspace,
        worker: usize,
    ) -> Result<workspace::DeliveryReport> {
        let accepted = self.accepted.as_ref().context(
            "This saved result predates explicit artifact accounting; export its recovery data before cleanup")?;
        workspace::deliver_accepted_result(prepared, worker, accepted)
    }

    pub fn deliver_with_validation(
        &self,
        prepared: &workspace::PreparedWorkspace,
        worker: usize,
        validate_inputs: impl FnOnce() -> Result<()>,
    ) -> Result<workspace::DeliveryReport> {
        let accepted = self.accepted.as_ref().context(
            "This saved result predates explicit artifact accounting; export its recovery data before cleanup")?;
        workspace::deliver_accepted_result_with_validation(
            prepared,
            worker,
            accepted,
            validate_inputs,
        )
    }
}

/// A command ID is evidence only when the bound native worker actually emitted
/// its terminal record for this revision. Native events do not contain an input
/// manifest, so these records do not claim that earlier checks used this exact
/// finish-time snapshot. The worker remains responsible for relevant checking.
pub(crate) fn validate_checks(
    declaration: &Value,
    native_checks: &HashMap<String, Value>,
    revision: u64,
) -> Result<Vec<Value>> {
    validate_records(declaration, revision, |id| {
        let native = native_checks.get(id).with_context(|| {
            format!("check command {id} was not observed from this native worker")
        })?;
        Ok((CommandEvidence::from_codex(native)?, native.clone()))
    })
}

pub(crate) fn validate_evidence(
    declaration: &Value,
    native_checks: &HashMap<String, CommandEvidence>,
    revision: u64,
) -> Result<Vec<Value>> {
    validate_records(declaration, revision, |id| {
        let evidence = native_checks.get(id).with_context(|| {
            format!("check command {id} was not observed from this native worker")
        })?;
        Ok((evidence.clone(), serde_json::to_value(evidence)?))
    })
}

fn validate_records(
    declaration: &Value,
    revision: u64,
    mut native_record: impl FnMut(&str) -> Result<(CommandEvidence, Value)>,
) -> Result<Vec<Value>> {
    ensure!(
        declaration["expected_revision"].as_u64() == Some(revision),
        "completion declaration is for an obsolete request revision"
    );
    let empty = Vec::new();
    let requested = match declaration.get("checks") {
        Some(value) => value
            .as_array()
            .context("checks must be an array of native command item IDs")?,
        None => &empty,
    };
    ensure!(requested.len() <= 64, "too many check references");
    let mut seen = HashSet::new();
    let mut verified = Vec::new();
    for reference in requested {
        let id = reference
            .as_str()
            .or_else(|| reference.get("id").and_then(Value::as_str))
            .context("check reference must be a native command item ID or {id,passed}")?;
        ensure!(seen.insert(id), "duplicate check command ID: {id}");
        let (evidence, native) = native_record(id)?;
        let passed = evidence.validate_for(id, revision)?;
        if let Some(claim) = reference.get("passed") {
            ensure!(
                claim.as_bool() == Some(passed),
                "claimed outcome for check {id} differs from its native completion"
            );
        }
        verified.push(
            json!({"item_id":id,"request_revision":revision,"passed":passed,"native":native,"evidence":evidence}),
        );
    }
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{CommandCompletion, FilesystemAccess, FilesystemScope, NativeHost};

    #[test]
    fn normalized_native_results_share_completion_fences_without_synthetic_exit_codes() {
        let (_temp, board, root, baseline) = board_project();
        std::fs::write(root.join("result.txt"), "ready").unwrap();
        let command = CommandEvidence {
            host: NativeHost::Claude,
            id: "bash-1".into(),
            command: "node --test".into(),
            cwd: root.clone(),
            revision: 1,
            started_sequence: Some(3),
            completion: CommandCompletion::NativeTool {
                result_ref: "8".into(),
                is_error: false,
                interrupted: false,
                background_task_id: None,
                timed_out: false,
            },
        };
        let mut checks = HashMap::from([("bash-1".into(), command)]);
        let declaration = json!({"outcome":"complete","expected_revision":1,"checks":[{"id":"bash-1","passed":true}]});
        let completion = Completion::capture_with_evidence(
            &board,
            1,
            &declaration,
            &checks,
            1,
            &workspace::ResultPolicy::default(),
            &baseline,
        )
        .unwrap();
        assert_eq!(completion.checks[0]["passed"], true);
        assert!(
            completion.checks[0]["native"]["completion"]
                .get("exit_code")
                .is_none()
        );
        completion.verify(&root).unwrap();
        completion.verify_shared_inputs(&board, 1).unwrap();
        let restored: Completion =
            serde_json::from_value(serde_json::to_value(&completion).unwrap()).unwrap();
        restored.verify(&root).unwrap();
        restored.verify_shared_inputs(&board, 1).unwrap();
        assert_eq!(restored.manifest, completion.manifest);
        if let CommandCompletion::NativeTool { is_error, .. } =
            &mut checks.get_mut("bash-1").unwrap().completion
        {
            *is_error = true;
        }
        assert!(validate_evidence(&declaration, &checks, 1).is_err());
        std::fs::write(root.join("result.txt"), "late change").unwrap();
        assert!(completion.verify(&root).is_err());
        assert!(restored.verify(&root).is_err());
    }

    fn board_project() -> (tempfile::TempDir, Board, std::path::PathBuf, Manifest) {
        let temp = tempfile::tempdir().unwrap();
        let run = temp.path().join("run");
        let baseline = temp.path().join("baseline");
        let workers = [temp.path().join("worker-1"), temp.path().join("worker-2")];
        for path in [&run, &baseline, &workers[0], &workers[1]] {
            std::fs::create_dir(path).unwrap();
        }
        let manifest = workspace::manifest(&baseline).unwrap();
        let mut board = Board::open(&run, &baseline, workers).unwrap();
        for worker in 1..=2 {
            let path = board.worker_path(worker).unwrap().to_path_buf();
            assert!(
                std::process::Command::new("git")
                    .args(["init", "--quiet"])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            board
                .set_worker_scopes(
                    worker,
                    vec![FilesystemScope {
                        path,
                        access: FilesystemAccess::Write,
                    }],
                )
                .unwrap();
        }
        let root = board.worker_path(1).unwrap().to_path_buf();
        (temp, board, root, manifest)
    }

    fn project() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .arg(root.path())
                .status()
                .unwrap()
                .success()
        );
        root
    }

    fn check(exit: i64) -> Value {
        json!({"type":"commandExecution","id":"check-1","command":"python3 test.py","cwd":"/private/project","status":if exit==0{"completed"}else{"failed"},"exitCode":exit,"aggregatedOutput":"retained native output","_delm_revision":4})
    }
    #[test]
    fn rejects_invented_stale_and_inaccurately_claimed_checks() {
        let mut native = HashMap::from([("check-1".to_owned(), check(1))]);
        assert!(
            validate_checks(
                &json!({"expected_revision":4,"checks":["invented"]}),
                &native,
                4
            )
            .is_err()
        );
        assert!(
            validate_checks(
                &json!({"expected_revision":5,"checks":["check-1"]}),
                &native,
                5
            )
            .is_err()
        );
        assert!(
            validate_checks(
                &json!({"expected_revision":4,"checks":[{"id":"check-1","passed":true}]}),
                &native,
                4
            )
            .is_err()
        );
        let actual = validate_checks(
            &json!({"expected_revision":4,"checks":["check-1"]}),
            &native,
            4,
        )
        .unwrap();
        assert_eq!(actual[0]["passed"], false);
        native.insert("check-1".into(), check(0));
        assert_eq!(
            validate_checks(
                &json!({"expected_revision":4,"checks":["check-1"]}),
                &native,
                4
            )
            .unwrap()[0]["passed"],
            true
        );
    }
    #[test]
    fn candidate_fence_detects_post_completion_writers_without_mandatory_checks() {
        let root = project();
        std::fs::write(root.path().join("result.txt"), "ready").unwrap();
        let declaration = json!({"outcome":"complete","expected_revision":4,"summary":"Updated prose; no executable behavior changed","checks":[]});
        let candidate = Completion::capture(
            root.path(),
            &declaration,
            &HashMap::new(),
            4,
            &workspace::ResultPolicy::default(),
        )
        .unwrap();
        candidate.verify(root.path()).unwrap();
        std::fs::write(root.path().join("result.txt"), "background change").unwrap();
        assert!(candidate.verify(root.path()).is_err());
    }

    #[test]
    fn shared_dependency_receipts_survive_serialization_and_retain_board_authority() {
        let (_temp, mut board, root, baseline) = board_project();
        for worker in 1..=2 {
            let path = board.worker_path(worker).unwrap();
            std::fs::write(path.join("result.txt"), "ready").unwrap();
            std::fs::create_dir_all(path.join("node_modules/library")).unwrap();
            std::fs::write(path.join("node_modules/library/index.js"), "dependency").unwrap();
        }
        let begun = board
            .begin_check(
                2,
                json!({"idempotency_key":"begin-peer","summary":"Focused peer check",
                    "paths":["result.txt","node_modules/library/index.js","optional.json"]}),
                || 10,
            )
            .unwrap();
        let command = CommandEvidence {
            host: NativeHost::Claude,
            id: "peer-check".into(),
            command: "node --test".into(),
            cwd: board.worker_path(2).unwrap().to_path_buf(),
            revision: 1,
            started_sequence: Some(11),
            completion: CommandCompletion::NativeTool {
                result_ref: "8".into(),
                is_error: false,
                interrupted: false,
                background_task_id: None,
                timed_out: false,
            },
        };
        let receipt = board
            .finish_check_with_evidence(
                2,
                json!({"idempotency_key":"finish-peer","snapshot_id":begun["result"]["snapshot_id"],
                    "command_id":"peer-check"}),
                &HashMap::from([("peer-check".into(), command)]),
            )
            .unwrap()["result"]
            .clone();
        let declaration = json!({"outcome":"complete","expected_revision":1,"summary":"Assembled contribution",
            "checks":[],"shared_checks":[receipt["receipt_id"]]});
        let candidate = Completion::capture_with_evidence(
            &board,
            1,
            &declaration,
            &HashMap::new(),
            1,
            &workspace::ResultPolicy::default(),
            &baseline,
        )
        .unwrap();
        assert!(candidate.checks.is_empty());
        assert_eq!(candidate.shared_checks[0]["worker"], 2);
        assert!(candidate.manifest.files.contains_key("result.txt"));
        assert!(
            !candidate
                .manifest
                .files
                .contains_key("node_modules/library/index.js")
        );
        let encoded = serde_json::to_value(&candidate).unwrap();
        let restored: Completion = serde_json::from_value(encoded.clone()).unwrap();
        restored.verify_shared_inputs(&board, 1).unwrap();
        let mut altered = encoded.clone();
        altered["shared_checks"][0]["summary"] = json!("unrecorded receipt");
        let altered: Completion = serde_json::from_value(altered).unwrap();
        assert!(altered.verify_shared_inputs(&board, 1).is_err());
        let mut altered = encoded.clone();
        altered["declaration"]["expected_revision"] = json!(2);
        let altered: Completion = serde_json::from_value(altered).unwrap();
        assert!(altered.verify_shared_inputs(&board, 1).is_err());
        let mut altered = encoded;
        altered["declaration"]["outcome"] = json!("waiting");
        let altered: Completion = serde_json::from_value(altered).unwrap();
        assert!(altered.verify_shared_inputs(&board, 1).is_err());
        std::fs::write(root.join("node_modules/library/unrelated.js"), "unrelated").unwrap();
        restored.verify_shared_inputs(&board, 1).unwrap();
        std::fs::write(root.join("optional.json"), "new input").unwrap();
        assert!(restored.verify_shared_inputs(&board, 1).is_err());
        std::fs::remove_file(root.join("optional.json")).unwrap();
        std::fs::write(root.join("node_modules/library/index.js"), "changed").unwrap();
        assert!(restored.verify_shared_inputs(&board, 1).is_err());
        assert!(
            Completion::capture_with_evidence(
                &board,
                1,
                &declaration,
                &HashMap::new(),
                1,
                &workspace::ResultPolicy::default(),
                &baseline,
            )
            .is_err()
        );
    }
}
