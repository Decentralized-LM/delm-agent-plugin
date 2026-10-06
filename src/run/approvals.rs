//! Correlated, user-supplied replies to native Codex approval requests.
//! Wire shapes follow the installed 0.160.0 app-server JSON schemas. DeLM does
//! not turn an approval request into permission, nor synthesize MCP proofs.
use crate::protocol::Event;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::HashMap;

pub(super) struct Pending {
    pub native_id: Value,
    pub worker: usize,
    pub turn: Option<String>,
    pub method: String,
    pub params: Value,
}

pub(super) struct Approvals(HashMap<String, Pending>, usize);
impl Default for Approvals {
    fn default() -> Self {
        Self(HashMap::new(), crate::config::DEFAULT_WORKER_COUNT)
    }
}

pub(crate) fn supported(method: &str) -> bool {
    matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "mcpServer/elicitation/request"
    )
}

fn object_keys(value: &Value, allowed: &[&str]) -> Result<()> {
    let object = value
        .as_object()
        .context("Approval response must be a JSON object")?;
    ensure!(
        object.keys().all(|key| allowed.contains(&key.as_str())),
        "Approval response contains unsupported fields"
    );
    Ok(())
}

/// Validate the response envelope and bind grants to this exact request.
/// MCP form contents and verification proofs remain opaque to DeLM: the native
/// host/server owns their schema and authenticity, and receives them unchanged.
pub(crate) fn validate_response(method: &str, params: &Value, response: &Value) -> Result<()> {
    ensure!(
        serde_json::to_vec(response)?.len() <= 256 * 1024,
        "Approval response exceeds 256 KiB"
    );
    match method {
        "item/commandExecution/requestApproval" => {
            object_keys(response, &["decision"])?;
            let decision = response
                .get("decision")
                .context("Approval decision is missing")?;
            if let Some(available) = params["availableDecisions"].as_array() {
                ensure!(
                    available.contains(decision),
                    "Decision was not offered for this command approval"
                );
            } else if let Some(choice) = decision.as_str() {
                ensure!(
                    ["accept", "acceptForSession", "decline", "cancel"].contains(&choice),
                    "Invalid command approval decision"
                );
            } else if let Some(amendment) = decision.get("acceptWithExecpolicyAmendment") {
                object_keys(decision, &["acceptWithExecpolicyAmendment"])?;
                object_keys(amendment, &["execpolicy_amendment"])?;
                let proposed = params
                    .get("proposedExecpolicyAmendment")
                    .filter(|v| v.is_array())
                    .context("No execution policy amendment was proposed")?;
                ensure!(
                    amendment.get("execpolicy_amendment") == Some(proposed),
                    "Execution policy amendment differs from the proposed rule"
                );
            } else if let Some(amendment) = decision.get("applyNetworkPolicyAmendment") {
                object_keys(decision, &["applyNetworkPolicyAmendment"])?;
                object_keys(amendment, &["network_policy_amendment"])?;
                let proposed = params["proposedNetworkPolicyAmendments"]
                    .as_array()
                    .context("No network policy amendment was proposed")?;
                ensure!(
                    amendment
                        .get("network_policy_amendment")
                        .is_some_and(|value| proposed.contains(value)),
                    "Network policy amendment was not offered"
                );
            } else {
                bail!("Invalid command approval decision");
            }
        }
        "item/fileChange/requestApproval" => {
            object_keys(response, &["decision"])?;
            ensure!(
                response["decision"].as_str().is_some_and(|s| [
                    "accept",
                    "acceptForSession",
                    "decline",
                    "cancel"
                ]
                .contains(&s)),
                "Invalid file-change approval decision"
            );
        }
        "item/permissions/requestApproval" => validate_permissions(params, response)?,
        "mcpServer/elicitation/request" => {
            object_keys(response, &["action", "content", "_meta"])?;
            let action = response["action"]
                .as_str()
                .context("Elicitation action is missing")?;
            ensure!(
                ["accept", "decline", "cancel"].contains(&action),
                "Invalid elicitation action"
            );
            if action != "accept" {
                ensure!(
                    response.get("content").is_none_or(Value::is_null),
                    "Decline/cancel cannot include accepted elicitation content"
                );
            } else if matches!(
                params["mode"].as_str(),
                Some("form" | "openai/form" | "openaiForm" | "openai/userVerification")
            ) {
                ensure!(
                    response.get("content").is_some_and(|v| !v.is_null()),
                    "Accepted form or verification requires the user's content"
                );
            }
        }
        _ => bail!("Unsupported native approval request: {method}"),
    }
    Ok(())
}

fn validate_permissions(params: &Value, response: &Value) -> Result<()> {
    object_keys(response, &["permissions", "scope", "strictAutoReview"])?;
    if let Some(scope) = response.get("scope") {
        ensure!(
            scope
                .as_str()
                .is_some_and(|s| ["turn", "session"].contains(&s)),
            "Permission scope must be turn or session"
        );
    }
    if let Some(review) = response.get("strictAutoReview") {
        ensure!(
            review.is_null() || review.is_boolean(),
            "strictAutoReview must be a boolean or null"
        );
    }
    let granted = response
        .get("permissions")
        .context("permissions is required; an empty object denies the request")?;
    object_keys(granted, &["network", "fileSystem"])?;
    if let Some(network) = granted.get("network").filter(|v| !v.is_null()) {
        object_keys(network, &["enabled"])?;
        if let Some(enabled) = network.get("enabled") {
            ensure!(
                enabled.is_null() || enabled.is_boolean(),
                "Network enabled must be a boolean or null"
            );
            ensure!(
                enabled != true || params["permissions"]["network"]["enabled"] == true,
                "Network permission was not requested"
            );
        }
    }
    let Some(files) = granted.get("fileSystem").filter(|v| !v.is_null()) else {
        return Ok(());
    };
    object_keys(files, &["read", "write", "entries", "globScanMaxDepth"])?;
    let requested = &params["permissions"]["fileSystem"];
    for field in ["read", "write"] {
        if let Some(paths) = files.get(field).filter(|v| !v.is_null()) {
            let paths = paths
                .as_array()
                .context("Granted filesystem paths must be arrays")?;
            for path in paths {
                ensure!(path.is_string(), "Filesystem path must be a string");
                let requested_here = requested[field]
                    .as_array()
                    .is_some_and(|values| values.contains(path));
                let requested_write = field == "read"
                    && requested["write"]
                        .as_array()
                        .is_some_and(|values| values.contains(path));
                ensure!(
                    requested_here || requested_write,
                    "Granted filesystem path was not requested"
                );
            }
        }
    }
    let entries = files
        .get("entries")
        .filter(|v| !v.is_null())
        .map(|v| v.as_array().context("Granted entries must be an array"))
        .transpose()?;
    if let Some(entries) = entries {
        let empty = Vec::new();
        let allowed = requested["entries"].as_array().unwrap_or(&empty);
        for entry in entries {
            object_keys(entry, &["path", "access"])?;
            ensure!(
                entry.get("path").is_some(),
                "Filesystem entry path is missing"
            );
            let access = entry["access"]
                .as_str()
                .context("Filesystem entry access is missing")?;
            ensure!(
                ["read", "write", "deny"].contains(&access),
                "Invalid filesystem access"
            );
            ensure!(
                allowed
                    .iter()
                    .any(|request| request["path"] == entry["path"]
                        && (request["access"] == access
                            || (request["access"] == "write" && access == "read"))),
                "Granted filesystem entry was not requested"
            );
        }
    }
    // Dropping a requested deny while approving a broader allow changes the
    // meaning of the request, so preserve its explicit restrictive entries.
    let grants_files = ["read", "write"]
        .iter()
        .any(|key| files[*key].as_array().is_some_and(|a| !a.is_empty()))
        || entries.is_some_and(|items| items.iter().any(|entry| entry["access"] != "deny"));
    if grants_files {
        if let Some(requested_entries) = requested["entries"].as_array() {
            for denied in requested_entries
                .iter()
                .filter(|entry| entry["access"] == "deny")
            {
                ensure!(
                    entries.is_some_and(|items| items.contains(denied)),
                    "Preserve the request's explicit denied paths when granting filesystem access"
                );
            }
        }
        if !requested["globScanMaxDepth"].is_null() {
            ensure!(
                files["globScanMaxDepth"] == requested["globScanMaxDepth"],
                "Preserve the requested glob scan depth"
            );
        }
    }
    if let Some(depth) = files.get("globScanMaxDepth").filter(|v| !v.is_null()) {
        ensure!(
            depth.as_u64().is_some_and(|n| n > 0) && depth == &requested["globScanMaxDepth"],
            "Glob scan depth was not requested"
        );
    }
    Ok(())
}

impl Approvals {
    pub fn for_worker_count(count: usize) -> Result<Self> {
        crate::config::validate_worker_count(count)?;
        Ok(Self(HashMap::new(), count))
    }

    pub fn insert(
        &mut self,
        native_id: Value,
        worker: usize,
        turn: Option<String>,
        method: &str,
        params: Value,
    ) -> Result<Event> {
        ensure!(
            worker < self.1 && supported(method),
            "Unbound or unsupported native approval"
        );
        ensure!(
            params.is_object(),
            "Native approval parameters must be an object"
        );
        ensure!(
            params["threadId"].as_str().is_some_and(|s| !s.is_empty()),
            "Native approval thread is missing"
        );
        ensure!(
            params["turnId"].as_str() == turn.as_deref(),
            "Native approval turn does not match its bound turn"
        );
        ensure!(
            method == "mcpServer/elicitation/request" || turn.is_some(),
            "Native approval must belong to a turn"
        );
        ensure!(
            !self
                .0
                .values()
                .any(|pending| pending.native_id == native_id),
            "Native approval request is already pending"
        );
        let id = uuid::Uuid::new_v4().to_string();
        let mut event = Event::new(
            "approval",
            format!("Worker {} needs your approval.", worker + 1),
        );
        event.id = Some(id.clone());
        event.details = Some(json!({"worker":worker+1,"method":method,"request":params}));
        self.0.insert(
            id,
            Pending {
                native_id,
                worker,
                turn,
                method: method.into(),
                params,
            },
        );
        Ok(event)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn take(&mut self, id: &str, response: &Value) -> Result<Pending> {
        let pending = self
            .0
            .get(id)
            .context("This native approval is no longer pending")?;
        validate_response(&pending.method, &pending.params, response)?;
        Ok(self.0.remove(id).expect("validated pending approval"))
    }

    pub fn resolve(&mut self, native_id: &Value) -> Option<Event> {
        let id = self
            .0
            .iter()
            .find(|(_, pending)| &pending.native_id == native_id)
            .map(|(id, _)| id.clone())?;
        self.0.remove(&id);
        Some(resolved(&id))
    }

    pub fn retire_turn(&mut self, worker: usize, turn: &str) -> Vec<Event> {
        self.retire(|pending| pending.worker == worker && pending.turn.as_deref() == Some(turn))
    }

    pub fn retire_worker(&mut self, worker: usize) -> Vec<Event> {
        self.retire(|pending| pending.worker == worker)
    }

    fn retire(&mut self, predicate: impl Fn(&Pending) -> bool) -> Vec<Event> {
        let ids = self
            .0
            .iter()
            .filter(|(_, pending)| predicate(pending))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        ids.into_iter()
            .map(|id| {
                self.0.remove(&id);
                resolved(&id)
            })
            .collect()
    }
}

pub(super) fn resolved(id: &str) -> Event {
    let mut event = Event::new(
        "approval_resolved",
        "The native approval is no longer pending.",
    );
    event.id = Some(id.into());
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    const COMMAND: &str = "item/commandExecution/requestApproval";
    const FILE: &str = "item/fileChange/requestApproval";
    const PERMISSIONS: &str = "item/permissions/requestApproval";
    const MCP: &str = "mcpServer/elicitation/request";

    #[test]
    fn command_choices_and_policy_amendments_are_request_bound() {
        let restricted = json!({"availableDecisions":["accept","decline"]});
        assert!(validate_response(COMMAND, &restricted, &json!({"decision":"accept"})).is_ok());
        assert!(
            validate_response(
                COMMAND,
                &restricted,
                &json!({"decision":"acceptForSession"})
            )
            .is_err()
        );
        let amendment =
            json!({"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["npm","install"]}});
        let proposed = json!({"proposedExecpolicyAmendment":["npm","install"]});
        assert!(validate_response(COMMAND, &proposed, &json!({"decision":amendment})).is_ok());
        assert!(validate_response(COMMAND,&proposed,&json!({"decision":{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["npm"]}}})).is_err());
        for decision in ["accept", "acceptForSession", "decline", "cancel"] {
            assert!(validate_response(FILE, &json!({}), &json!({"decision":decision})).is_ok());
        }
        assert!(validate_response(FILE, &json!({}), &json!({"decision":"approved"})).is_err());
    }

    #[test]
    fn permissions_allow_requested_subsets_but_never_expand_the_request() {
        let request = json!({"permissions":{"network":{"enabled":true},"fileSystem":{"read":["/project/data"],"write":["/project/output"]}}});
        assert!(
            validate_response(
                PERMISSIONS,
                &request,
                &json!({"permissions":{},"scope":"turn"})
            )
            .is_ok()
        );
        assert!(validate_response(PERMISSIONS,&request,&json!({"permissions":{"network":{"enabled":true},"fileSystem":{"read":["/project/output"]}},"scope":"session"})).is_ok());
        assert!(
            validate_response(
                PERMISSIONS,
                &request,
                &json!({"permissions":{"fileSystem":{"write":["/"]}}})
            )
            .is_err()
        );
        assert!(
            validate_response(
                PERMISSIONS,
                &json!({"permissions":{}}),
                &json!({"permissions":{"network":{"enabled":true}}})
            )
            .is_err()
        );
        let entries = json!({"permissions":{"fileSystem":{"entries":[{"path":{"type":"path","path":"/project"},"access":"write"},{"path":{"type":"path","path":"/project/private"},"access":"deny"}]}}});
        assert!(
            validate_response(
                PERMISSIONS,
                &entries,
                &json!({"permissions":entries["permissions"]})
            )
            .is_ok()
        );
        assert!(validate_response(PERMISSIONS,&entries,&json!({"permissions":{"fileSystem":{"entries":[entries["permissions"]["fileSystem"]["entries"][0]]}}})).is_err());
    }

    #[test]
    fn mcp_responses_preserve_forms_urls_and_opaque_verification_content() {
        for mode in [
            "form",
            "openai/form",
            "openaiForm",
            "openai/userVerification",
        ] {
            assert!(validate_response(MCP,&json!({"mode":mode}),&json!({"action":"accept","content":{"user_value":"entered"},"_meta":{"source":"client"}})).is_ok());
            assert!(
                validate_response(MCP, &json!({"mode":mode}), &json!({"action":"accept"})).is_err()
            );
        }
        assert!(
            validate_response(MCP, &json!({"mode":"url"}), &json!({"action":"accept"})).is_ok()
        );
        assert!(
            validate_response(
                MCP,
                &json!({"mode":"form"}),
                &json!({"action":"decline","content":null})
            )
            .is_ok()
        );
        assert!(
            validate_response(
                MCP,
                &json!({"mode":"form"}),
                &json!({"action":"decline","content":{"accepted":true}})
            )
            .is_err()
        );
    }

    #[test]
    fn independent_approvals_do_not_cross_workers_and_retired_turns_cannot_reply() {
        let mut approvals = Approvals::default();
        let params = json!({"threadId":"thread1","turnId":"turn1","availableDecisions":["accept","decline"]});
        let one = approvals
            .insert(json!(11), 0, Some("turn1".into()), COMMAND, params.clone())
            .unwrap();
        let two = approvals
            .insert(
                json!(12),
                1,
                Some("turn2".into()),
                FILE,
                json!({"threadId":"thread2","turnId":"turn2"}),
            )
            .unwrap();
        assert!(
            approvals
                .insert(json!(11), 0, Some("turn1".into()), COMMAND, params)
                .is_err()
        );
        assert!(
            approvals
                .take(one.id.as_ref().unwrap(), &json!({"decision":"cancel"}))
                .is_err()
        );
        let pending = approvals
            .take(one.id.as_ref().unwrap(), &json!({"decision":"accept"}))
            .unwrap();
        assert_eq!(pending.worker, 0);
        assert_eq!(pending.native_id, json!(11));
        assert!(
            approvals
                .take(one.id.as_ref().unwrap(), &json!({"decision":"accept"}))
                .is_err()
        );
        let retired = approvals.retire_turn(1, "turn2");
        assert_eq!(retired[0].id, two.id);
        assert!(approvals.is_empty());
        let standalone=approvals.insert(json!(13),1,None,MCP,json!({"threadId":"thread2","turnId":null,"mode":"url","message":"Authenticate","url":"https://example.com"})).unwrap();
        assert!(approvals.retire_turn(1, "other-turn").is_empty());
        assert_eq!(approvals.resolve(&json!(13)).unwrap().id, standalone.id);
        assert!(approvals.resolve(&json!(13)).is_none());
        approvals
            .insert(
                json!(14),
                1,
                None,
                MCP,
                json!({"threadId":"thread2","turnId":null,"mode":"url"}),
            )
            .unwrap();
        assert_eq!(approvals.retire_worker(1).len(), 1);
        assert!(approvals.is_empty());
    }
}
