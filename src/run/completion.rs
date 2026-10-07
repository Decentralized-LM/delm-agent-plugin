//! Codex completion keeps checked inputs separate from delivered files.
use crate::board::{Board, CompletionChecks};
use crate::workspace;
use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

pub(crate) use crate::completion::validate_checks;

/// Runtime-only evidence, verified through the same board that owns the worker
/// roots. Serialized reports keep the shared completion format; they are not a
/// means of reconstructing a Codex candidate for delivery.
#[derive(Serialize)]
#[serde(transparent)]
pub(super) struct Completion {
    evidence: crate::completion::Completion,
    #[serde(skip)]
    inputs: CompletionChecks,
    #[serde(skip)]
    worker: usize,
}

impl Completion {
    pub fn capture(
        board: &Board,
        worker: usize,
        declaration: &Value,
        native_checks: &HashMap<String, Value>,
        revision: u64,
        result_policy: &workspace::ResultPolicy,
        baseline: &workspace::Manifest,
    ) -> Result<Self> {
        let checks = validate_checks(declaration, native_checks, revision)?;
        let inputs = board.completion_checks(worker, declaration, revision)?;
        let evidence = crate::completion::Completion::capture_candidate(
            board.worker_path(worker)?,
            declaration,
            checks,
            revision,
            result_policy,
            inputs.receipts().to_vec(),
            baseline,
        )?;
        // Close the interval spent selecting outputs without treating omitted
        // environment inputs as missing from the worker's actual project.
        inputs.verify(board)?;
        Ok(Self {
            evidence,
            inputs,
            worker,
        })
    }

    pub fn revision(&self) -> u64 {
        self.evidence.revision
    }

    /// Call after every owned writer has stopped and before saving or delivering.
    pub fn verify(&self, board: &Board) -> Result<()> {
        self.evidence.verify(self.inputs.verify(board)?)?;
        board.worker_path(self.worker)?;
        Ok(())
    }

    pub fn deliver(
        &self,
        prepared: &workspace::PreparedWorkspace,
        worker: usize,
    ) -> Result<workspace::DeliveryReport> {
        ensure!(
            worker + 1 == self.worker,
            "completion belongs to another worker"
        );
        self.evidence.deliver(prepared, worker)
    }
}
