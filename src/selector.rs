//! Native Codex agent-count form. A model cannot supply the selected count.
//!
//! The command hook owns invocation capture. This transport only admits the
//! matching, independently validated capture after a native form response.
use crate::lifecycle::{self, CapturedInvocation, HookInput};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::time::Instant;

const MAX_MESSAGE: u64 = 1024 * 1024;
const CAPTURE_READY_TIMEOUT: Duration = Duration::from_secs(2);
const SELECTION_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_PENDING: usize = 16;

struct Pending {
    request_id: Value,
    capture: CapturedInvocation,
    deadline: Instant,
}

fn form_supported(initialize: &Value) -> bool {
    initialize
        .pointer("/params/capabilities/elicitation/form")
        .is_some_and(Value::is_object)
}

fn invocation_text(prompt: &str) -> bool {
    prompt
        .strip_prefix("$delm:run")
        .is_some_and(|tail| tail.starts_with(char::is_whitespace) && !tail.trim().is_empty())
}

fn explicit_invocation(input: &HookInput) -> bool {
    input.hook_event_name == "UserPromptSubmit"
        && input.agent_id.as_deref().is_none_or(str::is_empty)
        && input.prompt.as_deref().is_some_and(invocation_text)
}

fn tool_definition() -> Value {
    json!({"name":"select_agents", "description":"Open DeLM's required native agent-count selector for a trusted invocation. Only the user can choose the count.",
        "inputSchema":{"type":"object","additionalProperties":false,
            "properties":{"hook_event_name":{"type":"string"},"session_id":{"type":"string"},"turn_id":{"type":"string"},"prompt":{"type":"string"},"cwd":{"type":"string"}},
            "required":["hook_event_name","session_id","turn_id","prompt","cwd"]}})
}

fn form_request(id: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"elicitation/create","params":{
        "mode":"form","message":"How many agents?","requestedSchema":{
            "type":"object","properties":{"agents":{"type":"string","title":"Agents",
                "enum":["2","3","4"],"enumNames":["2 agents (default)","3 agents","4 agents"],"default":"2"}},
            "required":["agents"]}}})
}

fn selected_count(response: &Value) -> Result<Option<usize>> {
    ensure!(
        response.get("error").is_none(),
        "The native selection form failed"
    );
    let result = response
        .get("result")
        .context("The native selection response is missing")?;
    match result.get("action").and_then(Value::as_str) {
        Some("cancel" | "decline") => Ok(None),
        Some("accept") => {
            let content = result
                .get("content")
                .and_then(Value::as_object)
                .context("The native selection is missing")?;
            ensure!(
                content.len() == 1,
                "The native selection contains unexpected fields"
            );
            match content.get("agents").and_then(Value::as_str) {
                Some("2") => Ok(Some(2)),
                Some("3") => Ok(Some(3)),
                Some("4") => Ok(Some(4)),
                _ => anyhow::bail!("Choose exactly 2, 3, or 4 agents"),
            }
        }
        _ => anyhow::bail!("The native selection action is invalid"),
    }
}

fn hook_reply(id: Value, output: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":output.to_string()}]}})
}

fn stopped(id: Value, message: impl Into<String>) -> Value {
    hook_reply(id, json!({"continue":false,"stopReason":message.into()}))
}

fn cancel_form(id: String) -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{
        "requestId":id,"reason":"DeLM agent selection ended without launching another run"}})
}

async fn emit(stdout: &mut tokio::io::Stdout, value: Value) -> Result<()> {
    let mut encoded = serde_json::to_vec(&value)?;
    encoded.push(b'\n');
    stdout.write_all(&encoded).await?;
    stdout.flush().await?;
    Ok(())
}

async fn ready_capture(input: &HookInput, executable: &Path) -> Result<CapturedInvocation> {
    let deadline = Instant::now() + CAPTURE_READY_TIMEOUT;
    loop {
        if let Some(capture) = lifecycle::selection_capture(input, executable)? {
            return Ok(capture);
        }
        ensure!(
            Instant::now() < deadline,
            "DeLM's trusted invocation hook did not capture this request. Review DeLM in /hooks and restart Codex"
        );
        // Codex runs sibling hooks concurrently. This waits only for the local
        // command-hook write; it does not poll a worker, model or user decision.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn input_from_call(message: &Value) -> Result<HookInput> {
    ensure!(
        message.pointer("/params/name").and_then(Value::as_str) == Some("select_agents"),
        "Unknown DeLM selector tool"
    );
    let arguments = message
        .pointer("/params/arguments")
        .context("Missing native hook input")?;
    let object = arguments
        .as_object()
        .context("Native hook input must be an object")?;
    ensure!(
        object.keys().all(|key| matches!(
            key.as_str(),
            "hook_event_name" | "session_id" | "turn_id" | "prompt" | "cwd"
        )),
        "Unexpected selector input; the agent count must come from the native form"
    );
    let input: HookInput = serde_json::from_value(arguments.clone())?;
    ensure!(
        message
            .pointer("/params/_meta/threadId")
            .and_then(Value::as_str)
            == Some(input.session_id.as_str()),
        "Native selector thread identity is missing or changed"
    );
    Ok(input)
}

fn cancel(pending: &Pending, executable: &Path) -> Result<()> {
    lifecycle::cancel_selection(&pending.capture, executable)
}

pub async fn serve_stdio() -> Result<()> {
    let executable = std::env::current_exe()?;
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut buffer = Vec::new();
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut supports_form = false;
    let result: Result<()> = async {
        loop {
            let deadline = pending.values().map(|request| request.deadline).min()
                .unwrap_or_else(|| Instant::now() + SELECTION_TIMEOUT);
            let mut limited = (&mut stdin).take(MAX_MESSAGE + 1 - buffer.len() as u64);
            tokio::select! {
                result = limited.read_until(b'\n', &mut buffer) => {
                    let count = result?;
                    if count == 0 { break; }
                    ensure!(buffer.len() as u64 <= MAX_MESSAGE, "Native selector message exceeds 1 MiB");
                    let message: Value = serde_json::from_slice(&buffer)?;
                    buffer.clear();
                    let id = message.get("id").cloned();
                    match message.get("method").and_then(Value::as_str) {
                        Some("initialize") => {
                            supports_form = form_supported(&message);
                            emit(&mut stdout,json!({"jsonrpc":"2.0","id":id,"result":{
                                "protocolVersion":message.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or("2025-03-26"),
                                "capabilities":{"tools":{}},"serverInfo":{"name":"delm-selector","version":env!("CARGO_PKG_VERSION")}}})).await?;
                        }
                        Some("tools/list") => emit(&mut stdout,json!({"jsonrpc":"2.0","id":id,"result":{"tools":[tool_definition()]}})).await?,
                        Some("ping") => emit(&mut stdout,json!({"jsonrpc":"2.0","id":id,"result":{}})).await?,
                        Some("tools/call") => {
                            let id = id.context("Native selector call omitted its request identity")?;
                            // A selector failure must never intercept ordinary
                            // conversation, even when optional host metadata is
                            // absent. Only an explicit invocation needs admission.
                            if std::env::var_os("DELM_WORKER_SESSION").is_some()
                                || !message.pointer("/params/arguments/prompt").and_then(Value::as_str).is_some_and(invocation_text) {
                                emit(&mut stdout,hook_reply(id,json!({}))).await?;
                                continue;
                            }
                            let input = match input_from_call(&message) {
                                Ok(input) => input,
                                Err(error) => { emit(&mut stdout,stopped(id,format!("DeLM could not open its selector: {error:#}. No agents started."))).await?; continue; }
                            };
                            if !explicit_invocation(&input) { emit(&mut stdout,hook_reply(id,json!({}))).await?; continue; }
                            if pending.values().any(|request| request.capture.session_id == input.session_id && Some(&request.capture.turn_id) == input.turn_id.as_ref()) {
                                emit(&mut stdout,stopped(id,"DeLM's agent selection is already open for this request. No second run was started.")).await?;
                                continue;
                            }
                            if pending.len() >= MAX_PENDING {
                                emit(&mut stdout,stopped(id,"Too many DeLM selections are already open. Finish or cancel one before invoking DeLM again.")).await?;
                                continue;
                            }
                            let capture = match ready_capture(&input,&executable).await {
                                Ok(capture) => capture,
                                Err(error) => { emit(&mut stdout,stopped(id,format!("DeLM could not open its selector: {error:#}. No agents started."))).await?; continue; }
                            };
                            let request = Pending {request_id:id.clone(),capture,deadline:Instant::now()+SELECTION_TIMEOUT};
                            if !supports_form {
                                let cancelled = cancel(&request,&executable);
                                let reason = cancelled.err().map(|error|format!(" Cancellation needs attention: {error:#}.")).unwrap_or_default();
                                emit(&mut stdout,stopped(id,format!("This Codex session cannot show the required agent selector. No agents started.{reason}"))).await?;
                                continue;
                            }
                            let form_id = format!("delm-selection-{}",uuid::Uuid::new_v4());
                            pending.insert(form_id.clone(),request);
                            emit(&mut stdout,form_request(&form_id)).await?;
                        }
                        Some("notifications/cancelled") => {
                            if let Some(cancel_id) = message.pointer("/params/requestId") {
                                let form_id = pending.iter().find(|(_,request)| &request.request_id == cancel_id).map(|(id,_)| id.clone());
                                if let Some(form_id) = form_id {
                                    let request = pending.remove(&form_id).unwrap();
                                    let error = cancel(&request,&executable).err().map(|error|format!(" Cancellation needs attention: {error:#}.")).unwrap_or_default();
                                    emit(&mut stdout,cancel_form(form_id)).await?;
                                    emit(&mut stdout,stopped(request.request_id,format!("DeLM selection cancelled. No agents started.{error}"))).await?;
                                }
                            }
                        }
                        Some(method) if method.starts_with("notifications/") => {},
                        Some(_) => if let Some(id) = id { emit(&mut stdout,json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Unknown method"}})).await?; },
                        None => {
                            let Some(form_id) = id.as_ref().and_then(Value::as_str) else { continue; };
                            let Some(request) = pending.remove(form_id) else { continue; };
                            let outcome = if Instant::now() >= request.deadline { Err(anyhow::anyhow!("The agent selection expired; invoke DeLM again")) } else { selected_count(&message) };
                            let output = match outcome {
                                Ok(Some(count)) => {
                                    // The native command hook observes this
                                    // durable confirmation and remains the sole
                                    // launcher, preserving its host environment.
                                    match lifecycle::confirm_selection(&request.capture,&executable,count) {
                                        Ok(capture) => lifecycle::selection_context(&capture,&executable),
                                        Err(error) => { let _ = cancel(&request,&executable); json!({"continue":false,"stopReason":format!("DeLM could not confirm this selection: {error:#}. No additional agents were started.")}) }
                                    }
                                }
                                Ok(None) => {
                                    let error = cancel(&request,&executable).err().map(|error|format!(" Cancellation needs attention: {error:#}.")).unwrap_or_default();
                                    let reason = if message.pointer("/result/action").and_then(Value::as_str) == Some("decline") {
                                        "DeLM selection was declined or disabled by Codex. No agents started. Use an interactive Codex session that allows questions, such as --ask-for-approval on-request."
                                    } else {
                                        "DeLM selection cancelled. No agents started."
                                    };
                                    json!({"continue":false,"stopReason":format!("{reason}{error}")})
                                }
                                Err(error) => {
                                    let _ = cancel(&request,&executable);
                                    json!({"continue":false,"stopReason":format!("DeLM selection failed: {error:#}. No agents started.")})
                                }
                            };
                            emit(&mut stdout,hook_reply(request.request_id,output)).await?;
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline), if !pending.is_empty() => {
                    let expired = pending.iter().filter(|(_,request)| request.deadline <= Instant::now()).map(|(id,_)|id.clone()).collect::<Vec<_>>();
                    for id in expired {
                        let request = pending.remove(&id).unwrap();
                        let _ = cancel(&request,&executable);
                        emit(&mut stdout,cancel_form(id)).await?;
                        emit(&mut stdout,stopped(request.request_id,"DeLM selection expired. No agents started. Invoke DeLM again to choose an agent count.")).await?;
                    }
                }
            }
        }
        Ok(())
    }.await;
    for request in pending.values() {
        let _ = cancel(request, &executable);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_native_choices_are_accepted() {
        for count in ["2", "3", "4"] {
            assert_eq!(
                selected_count(&json!({"result":{"action":"accept","content":{"agents":count}}}))
                    .unwrap(),
                Some(count.parse().unwrap())
            );
        }
        for value in [
            json!(2),
            json!("1"),
            json!("5"),
            json!("four"),
            json!("04"),
            json!(null),
        ] {
            assert!(
                selected_count(&json!({"result":{"action":"accept","content":{"agents":value}}}))
                    .is_err()
            );
        }
        assert!(
            selected_count(
                &json!({"result":{"action":"accept","content":{"agents":"2","extra":true}}})
            )
            .is_err()
        );
        assert!(selected_count(&json!({"error":{"message":"closed"}})).is_err());
        for action in ["cancel", "decline"] {
            assert_eq!(
                selected_count(&json!({"result":{"action":action}})).unwrap(),
                None
            );
        }
    }

    #[test]
    fn form_is_required_and_defaults_to_two_every_time() {
        let form = form_request("proof");
        assert_eq!(
            form.pointer("/params/requestedSchema/properties/agents/default"),
            Some(&json!("2"))
        );
        assert_eq!(
            form.pointer("/params/requestedSchema/required"),
            Some(&json!(["agents"]))
        );
        assert_eq!(
            form.pointer("/params/requestedSchema/properties/agents/enum"),
            Some(&json!(["2", "3", "4"]))
        );
        assert!(!form_supported(&json!({"params":{"capabilities":{}}})));
        assert!(form_supported(
            &json!({"params":{"capabilities":{"elicitation":{"form":{}}}}})
        ));
    }

    #[test]
    fn tool_input_cannot_supply_count_or_another_thread() {
        let mut call = json!({"params":{"name":"select_agents","arguments":{
            "hook_event_name":"UserPromptSubmit","session_id":"thread","turn_id":"turn","prompt":"$delm:run Build three pages","cwd":"/project"},"_meta":{"threadId":"thread"}}});
        assert!(explicit_invocation(&input_from_call(&call).unwrap()));
        call["params"]["arguments"]["count"] = json!(4);
        assert!(input_from_call(&call).is_err());
        call["params"]["arguments"]
            .as_object_mut()
            .unwrap()
            .remove("count");
        call["params"]["_meta"]["threadId"] = json!("another");
        assert!(input_from_call(&call).is_err());
    }
}
