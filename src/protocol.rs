use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartRequest {
    pub project: PathBuf,
    pub task: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub attachments: Vec<Value>,
    pub model: String,
    #[serde(default = "openai")]
    pub model_provider: String,
    pub reasoning_effort: Option<String>,
    pub service_tier: Option<String>,
    pub auth_home: PathBuf,
    #[serde(default = "empty_settings")]
    pub auth_settings: Value,
    pub host_executable: PathBuf,
    #[serde(default = "default_seconds")]
    pub seconds: u64,
    pub policy: Value,
}
fn openai() -> String {
    "openai".into()
}
fn empty_settings() -> Value {
    serde_json::json!({})
}
fn default_seconds() -> u64 {
    crate::config::DEFAULT_RUN_SECONDS
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostCommand {
    Start(StartRequest),
    Message {
        text: String,
        #[serde(default)]
        attachments: Vec<Value>,
    },
    Answer {
        id: String,
        answers: std::collections::BTreeMap<String, Vec<String>>,
    },
    Stop,
    AcceptResult {
        request_revision: u64,
    },
    Approval {
        id: String,
        decision: String,
    },
    Respond {
        id: String,
        response: Value,
    },
    Resume {
        run_id: String,
        authorization: StartRequest,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial_paths: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}
impl Event {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            path: None,
            run_id: None,
            id: None,
            partial_paths: Vec::new(),
            request_revision: None,
            details: None,
        }
    }
}
