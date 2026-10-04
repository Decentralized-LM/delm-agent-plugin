//! Runtime evidence for a normally completed native turn. This establishes
//! artifact identity and truthful command references, not semantic correctness.
use crate::workspace::{self, Manifest};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Completion {
    pub revision: u64,
    pub declaration: Value,
    pub checks: Vec<Value>,
    #[serde(default)]
    pub shared_checks: Vec<Value>,
    pub manifest: Manifest,
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
        )
    }

    /// The board validates receipt provenance against the bound worker before
    /// this call. Match the receipt's files to the captured candidate as well,
    /// closing the interval between board validation and candidate capture.
    pub fn capture_with_shared(
        root: &Path,
        declaration: &Value,
        native_checks: &HashMap<String, Value>,
        revision: u64,
        result_policy: &workspace::ResultPolicy,
        shared_checks: Vec<Value>,
    ) -> Result<Self> {
        ensure!(
            declaration["outcome"].as_str() == Some("complete"),
            "candidate must declare the whole request complete"
        );
        ensure!(
            declaration["expected_revision"].as_u64() == Some(revision),
            "candidate declaration is for an obsolete request revision"
        );
        let checks = validate_checks(declaration, native_checks, revision)?;
        let manifest = workspace::manifest_for_result(root, result_policy)?;
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
            for (path, version) in receipt["files"]
                .as_object()
                .context("receipt has no input scope")?
            {
                let entry = manifest.files.get(path);
                if version.is_null() {
                    ensure!(entry.is_none(), "shared check input changed at {path}");
                } else {
                    let entry = entry.with_context(|| {
                        format!("shared check input is absent from candidate: {path}")
                    })?;
                    ensure!(
                        entry.kind == workspace::FileKind::File
                            && entry.sha256.as_deref() == version["sha256"].as_str()
                            && Some(entry.size) == version["bytes"].as_u64()
                            && Some(u64::from(entry.mode & 0o111))
                                == version["executable"].as_u64(),
                        "shared check input changed at {path}"
                    );
                }
            }
        }
        Ok(Self {
            revision,
            declaration: declaration.clone(),
            checks,
            shared_checks,
            manifest,
            result_policy: result_policy.clone(),
        })
    }

    /// Call after all owned writers have stopped, before retaining or cleaning.
    pub fn verify(&self, root: &Path) -> Result<()> {
        ensure!(
            workspace::manifest_for_result(root, &self.result_policy)? == self.manifest,
            "candidate files changed between native completion and stopped-writer verification; preserve both workspaces without publishing this result"
        );
        Ok(())
    }
}

/// A command ID is evidence only when the bound native worker actually emitted
/// its terminal record for this revision. Native events do not contain an input
/// manifest, so these records do not claim that earlier checks used this exact
/// finish-time snapshot. The worker remains responsible for relevant checking.
pub(super) fn validate_checks(
    declaration: &Value,
    native_checks: &HashMap<String, Value>,
    revision: u64,
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
        let command = native_checks.get(id).with_context(|| {
            format!("check command {id} was not observed from this native worker")
        })?;
        ensure!(
            command["type"].as_str() == Some("commandExecution")
                && command["id"].as_str() == Some(id),
            "invalid native check record {id}"
        );
        ensure!(
            command["_delm_revision"].as_u64() == Some(revision),
            "check command {id} belongs to an obsolete or unbound request revision"
        );
        ensure!(
            matches!(command["status"].as_str(), Some("completed" | "failed")),
            "check command {id} has not finished execution"
        );
        let exit = command["exitCode"]
            .as_i64()
            .with_context(|| format!("check command {id} has no observed exit status"))?;
        ensure!(
            command["command"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "check command {id} omitted its command"
        );
        ensure!(
            command["cwd"]
                .as_str()
                .is_some_and(|cwd| Path::new(cwd).is_absolute()),
            "check command {id} omitted its execution directory"
        );
        let passed = exit == 0 && command["status"].as_str() == Some("completed");
        if let Some(claim) = reference.get("passed") {
            ensure!(
                claim.as_bool() == Some(passed),
                "claimed outcome for check {id} differs from its native exit status"
            );
        }
        verified.push(
            json!({"item_id":id,"request_revision":revision,"passed":passed,"native":command}),
        );
    }
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let root = tempfile::tempdir().unwrap();
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
    fn shared_receipts_match_the_captured_candidate_without_forging_local_commands() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("result.txt"), "ready").unwrap();
        let manifest = workspace::manifest(root.path()).unwrap();
        let file = &manifest.files["result.txt"];
        let receipt = json!({"receipt_id":12,"worker":2,"request_revision":4,"reusable":true,
            "files":{"result.txt":{"sha256":file.sha256,"bytes":file.size,"executable":file.mode & 0o111}},
            "native":{"id":"peer-check"}});
        let declaration = json!({"outcome":"complete","expected_revision":4,"summary":"Assembled contribution","checks":[],"shared_checks":[12]});
        assert!(
            Completion::capture(
                root.path(),
                &declaration,
                &HashMap::new(),
                4,
                &workspace::ResultPolicy::default()
            )
            .is_err()
        );
        let candidate = Completion::capture_with_shared(
            root.path(),
            &declaration,
            &HashMap::new(),
            4,
            &workspace::ResultPolicy::default(),
            vec![receipt.clone()],
        )
        .unwrap();
        assert!(candidate.checks.is_empty());
        assert_eq!(candidate.shared_checks[0]["worker"], 2);
        std::fs::write(root.path().join("result.txt"), "changed").unwrap();
        assert!(
            Completion::capture_with_shared(
                root.path(),
                &declaration,
                &HashMap::new(),
                4,
                &workspace::ResultPolicy::default(),
                vec![receipt]
            )
            .is_err()
        );
    }
}
