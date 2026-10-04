//! Host-neutral records derived by trusted native adapters, never by a model's
//! coordination arguments. A tool result is not a fabricated process exit.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeHost {
    Codex,
    Claude,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Completed,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandCompletion {
    NativeProcess {
        status: ProcessStatus,
        exit_code: i64,
    },
    NativeTool {
        result_ref: String,
        is_error: bool,
        interrupted: bool,
        background_task_id: Option<String>,
        timed_out: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandEvidence {
    pub host: NativeHost,
    pub id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub revision: u64,
    pub started_sequence: Option<u64>,
    pub completion: CommandCompletion,
}

impl CommandEvidence {
    /// Validate terminal native evidence and return its observed outcome.
    /// Neither branch establishes input coverage or semantic correctness.
    pub fn validate_for(&self, id: &str, revision: u64) -> Result<bool> {
        ensure!(
            !id.is_empty() && self.id == id,
            "invalid native command identity"
        );
        ensure!(
            revision > 0 && self.revision == revision,
            "command belongs to an obsolete or unbound request revision"
        );
        ensure!(
            !self.command.trim().is_empty(),
            "native command omitted its command text"
        );
        ensure!(
            self.cwd.is_absolute(),
            "native command omitted its execution directory"
        );
        match &self.completion {
            CommandCompletion::NativeProcess { status, exit_code } => {
                Ok(*status == ProcessStatus::Completed && *exit_code == 0)
            }
            CommandCompletion::NativeTool {
                result_ref,
                is_error,
                interrupted,
                background_task_id,
                timed_out,
            } => {
                ensure!(
                    !result_ref.trim().is_empty(),
                    "native tool result reference is missing"
                );
                ensure!(
                    !interrupted && !timed_out && background_task_id.is_none(),
                    "native tool has no completed foreground result"
                );
                Ok(!is_error)
            }
        }
    }

    /// Compatibility adapter for the existing Codex native event ledger.
    pub fn from_codex(record: &Value) -> Result<Self> {
        ensure!(
            record["type"] == "commandExecution",
            "invalid native command record"
        );
        let status = match record["status"].as_str() {
            Some("completed") => ProcessStatus::Completed,
            Some("failed") => ProcessStatus::Failed,
            _ => anyhow::bail!("native command has not finished execution"),
        };
        Ok(Self {
            host: NativeHost::Codex,
            id: record["id"]
                .as_str()
                .context("native command identity missing")?
                .into(),
            command: record["command"]
                .as_str()
                .context("native command omitted its command text")?
                .into(),
            cwd: PathBuf::from(
                record["cwd"]
                    .as_str()
                    .context("native command omitted its cwd")?,
            ),
            revision: record["_delm_revision"]
                .as_u64()
                .context("native command has no bound request revision")?,
            started_sequence: record["_delm_started_sequence"].as_u64(),
            completion: CommandCompletion::NativeProcess {
                status,
                exit_code: record["exitCode"]
                    .as_i64()
                    .context("native command has no exit status")?,
            },
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemAccess {
    Read,
    Write,
    Deny,
}

impl FilesystemAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Deny => "deny",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilesystemScope {
    pub path: PathBuf,
    pub access: FilesystemAccess,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> CommandEvidence {
        CommandEvidence {
            host: NativeHost::Claude,
            id: "bash-1".into(),
            command: "node --test".into(),
            cwd: "/project".into(),
            revision: 1,
            started_sequence: Some(4),
            completion: CommandCompletion::NativeTool {
                result_ref: "result-1".into(),
                is_error: false,
                interrupted: false,
                background_task_id: None,
                timed_out: false,
            },
        }
    }

    #[test]
    fn foreground_tool_evidence_retains_its_provenance_without_inventing_an_exit_code() {
        let mut evidence = tool();
        assert!(evidence.validate_for("bash-1", 1).unwrap());
        let json = serde_json::to_value(&evidence).unwrap();
        assert_eq!(json["completion"]["kind"], "native_tool");
        assert!(json["completion"].get("exit_code").is_none());
        if let CommandCompletion::NativeTool { is_error, .. } = &mut evidence.completion {
            *is_error = true;
        }
        assert!(!evidence.validate_for("bash-1", 1).unwrap());
    }

    #[test]
    fn interrupted_background_timeout_and_unreferenced_tool_results_are_not_checks() {
        for case in 0..4 {
            let mut evidence = tool();
            if let CommandCompletion::NativeTool {
                result_ref,
                interrupted,
                background_task_id,
                timed_out,
                ..
            } = &mut evidence.completion
            {
                match case {
                    0 => *interrupted = true,
                    1 => *background_task_id = Some("task-1".into()),
                    2 => *timed_out = true,
                    _ => result_ref.clear(),
                }
            }
            assert!(evidence.validate_for("bash-1", 1).is_err());
        }
        assert!(tool().validate_for("another-call", 1).is_err());
        assert!(tool().validate_for("bash-1", 2).is_err());
    }
}
