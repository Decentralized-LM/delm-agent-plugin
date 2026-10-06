//! Capability checks for the installed Codex, independent of its release number.
//! The probe uses disposable files and never starts a model turn.
use crate::protocol::StartRequest;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::process::Command;

struct Probe(PathBuf);
impl Probe {
    fn new() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("delm-compatibility-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path.canonicalize()?))
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub async fn host_version(host: &Path) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(host)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Codex version check timed out")??;
    let version = String::from_utf8(output.stdout).context("Codex returned an invalid version")?;
    ensure!(
        output.status.success() && !version.trim().is_empty(),
        "Could not identify the installed Codex"
    );
    Ok(version.trim().to_owned())
}

// Check the protocol we actually use, not an entire versioned schema snapshot.
// Extra methods, parameters and response fields are intentionally accepted.
const CLIENT_METHODS: &[(&str, &[&str])] = &[
    (
        "thread/fork",
        &[
            "threadId",
            "cwd",
            "config",
            "developerInstructions",
            "beforeTurnId",
            "deferGoalContinuation",
            "excludeTurns",
            "ephemeral",
            "runtimeWorkspaceRoots",
        ],
    ),
    (
        "thread/resume",
        &["threadId", "cwd", "config", "runtimeWorkspaceRoots"],
    ),
    ("thread/read", &["threadId", "includeTurns"]),
    ("turn/start", &["threadId", "input"]),
    ("turn/steer", &["threadId", "expectedTurnId", "input"]),
    ("turn/interrupt", &["threadId", "turnId"]),
    ("thread/backgroundTerminals/clean", &["threadId"]),
    ("thread/archive", &["threadId"]),
    (
        "thread/start",
        &[
            "cwd",
            "model",
            "modelProvider",
            "serviceTier",
            "config",
            "developerInstructions",
            "dynamicTools",
            "ephemeral",
            "environments",
            "runtimeWorkspaceRoots",
            "approvalPolicy",
            "sandbox",
        ],
    ),
    ("thread/unsubscribe", &["threadId"]),
    ("config/read", &["cwd", "includeLayers"]),
    ("configRequirements/read", &[]),
    ("plugin/reconcile", &["reason"]),
    ("skills/list", &["cwds", "forceReload"]),
    ("hooks/list", &["cwds"]),
    ("skills/extraRoots/set", &["extraRoots"]),
    (
        "mcpServerStatus/list",
        &["threadId", "detail", "limit", "cursor"],
    ),
];

const SERVER_METHODS: &[(&str, &[&str])] = &[
    (
        "item/tool/call",
        &["threadId", "turnId", "callId", "tool", "arguments"],
    ),
    (
        "item/tool/requestUserInput",
        &["threadId", "turnId", "itemId", "questions"],
    ),
    (
        "item/commandExecution/requestApproval",
        &["threadId", "turnId", "itemId", "availableDecisions"],
    ),
    (
        "item/fileChange/requestApproval",
        &["threadId", "turnId", "itemId"],
    ),
    (
        "item/permissions/requestApproval",
        &["threadId", "turnId", "itemId", "permissions"],
    ),
    (
        "mcpServer/elicitation/request",
        &["threadId", "turnId", "serverName"],
    ),
];

fn method_params<'a>(schema: &'a Value, method: &str) -> Result<&'a Value> {
    let variants = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
        .context("Codex omitted protocol method variants")?;
    let variant = variants
        .iter()
        .find(|variant| {
            let name = &variant["properties"]["method"];
            name["const"].as_str() == Some(method)
                || name["enum"]
                    .as_array()
                    .is_some_and(|names| names.iter().any(|name| name.as_str() == Some(method)))
        })
        .with_context(|| format!("Codex is missing required method {method}"))?;
    let params = &variant["properties"]["params"];
    if let Some(reference) = params["$ref"].as_str() {
        schema
            .pointer(
                reference
                    .strip_prefix('#')
                    .context("Codex exported an unsupported external parameter reference")?,
            )
            .with_context(|| format!("Codex omitted parameters for {method}"))
    } else {
        Ok(params)
    }
}

fn check_method(schema: &Value, method: &str, fields: &[&str], client: bool) -> Result<()> {
    let params = method_params(schema, method)?;
    if fields.is_empty() {
        ensure!(
            accepts_wire_value(schema, params, &Value::Null, 0)
                || accepts_wire_value(schema, params, &json!({}), 0),
            "Codex changed the zero-argument format of {method}"
        );
        return Ok(());
    }
    let properties = params["properties"]
        .as_object()
        .with_context(|| format!("Codex omitted parameter evidence for {method}"))?;
    for field in fields {
        ensure!(
            properties.contains_key(*field),
            "Codex is missing required capability {method}.{field}"
        );
        // Server additions are harmless, but identifiers used for routing must
        // retain their string representation just like outgoing parameters.
        if client || matches!(*field, "threadId" | "turnId" | "callId" | "itemId" | "tool") {
            let sample = wire_sample(field);
            ensure!(
                accepts_wire_value(schema, &properties[*field], &sample, 0),
                "Codex changed the required wire format of {method}.{field}"
            );
        }
    }
    if client && let Some(required) = params["required"].as_array() {
        for field in required {
            ensure!(
                field.as_str().is_some_and(|field| fields.contains(&field)),
                "Codex requires an unsupported parameter for {method}: {field}"
            );
        }
    }
    Ok(())
}

// These are the concrete JSON shapes DeLM sends, not a general JSON Schema
// validator. Live thread/configuration and sandbox checks remain authoritative.
fn wire_sample(field: &str) -> Value {
    match field {
        "ephemeral"
        | "includeTurns"
        | "excludeTurns"
        | "deferGoalContinuation"
        | "includeLayers"
        | "forceReload" => json!(true),
        "config" => json!({}),
        "dynamicTools" => json!(crate::board::tool_definitions()),
        "input" => json!([crate::workers::text_input("compatibility probe")]),
        "environments" => json!([{"environmentId":"local","cwd":"/private/tmp",
            "runtimeWorkspaceRoots":["/private/tmp"]}]),
        "runtimeWorkspaceRoots" | "cwds" | "extraRoots" => json!(["/private/tmp"]),
        "command" => json!(["/bin/sh", "-c", "true"]),
        "timeoutMs" | "outputBytesCap" | "limit" => json!(100),
        "permissions" | "permissionProfile" => json!("delm_worker_1"),
        "modelProvider" => json!("openai"),
        "serviceTier" => json!("fast"),
        "approvalPolicy" => json!("on-request"),
        "sandbox" => json!("workspace-write"),
        "detail" => json!("toolsAndAuthOnly"),
        "cwd" => json!("/private/tmp"),
        _ => json!("compatibility-probe"),
    }
}

fn accepts_wire_value(root: &Value, schema: &Value, value: &Value, depth: usize) -> bool {
    if depth > 24 || schema == &json!(false) {
        return false;
    }
    if let Some(reference) = schema["$ref"].as_str() {
        return reference
            .strip_prefix('#')
            .and_then(|pointer| root.pointer(pointer))
            .is_some_and(|node| accepts_wire_value(root, node, value, depth + 1));
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(variants) = schema[key].as_array()
            && !variants
                .iter()
                .any(|node| accepts_wire_value(root, node, value, depth + 1))
        {
            return false;
        }
    }
    if let Some(parts) = schema["allOf"].as_array()
        && !parts
            .iter()
            .all(|node| accepts_wire_value(root, node, value, depth + 1))
    {
        return false;
    }
    if let Some(constant) = schema.get("const")
        && constant != value
    {
        return false;
    }
    if let Some(variants) = schema["enum"].as_array()
        && !variants.contains(value)
    {
        return false;
    }
    let matches_type = |kind: &str| match kind {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "null" => value.is_null(),
        _ => false,
    };
    if let Some(kind) = schema.get("type") {
        let allowed = kind.as_str().is_some_and(matches_type)
            || kind.as_array().is_some_and(|kinds| {
                kinds
                    .iter()
                    .any(|kind| kind.as_str().is_some_and(matches_type))
            });
        if !allowed {
            return false;
        }
    }
    if let Some(values) = value.as_array()
        && let Some(items) = schema.get("items")
        && !values
            .iter()
            .all(|value| accepts_wire_value(root, items, value, depth + 1))
    {
        return false;
    }
    if let Some(values) = value.as_object() {
        if let Some(required) = schema["required"].as_array()
            && !required
                .iter()
                .all(|key| key.as_str().is_some_and(|key| values.contains_key(key)))
        {
            return false;
        }
        if let Some(properties) = schema["properties"].as_object()
            && !values.iter().all(|(key, value)| {
                properties
                    .get(key)
                    .is_none_or(|node| accepts_wire_value(root, node, value, depth + 1))
            })
        {
            return false;
        }
    }
    true
}

fn check_mcp_item_notification(schema: &Value) -> Result<()> {
    check_method(
        schema,
        "item/started",
        &["threadId", "turnId", "item"],
        false,
    )?;
    let params = method_params(schema, "item/started")?;
    let item = &params["properties"]["item"];
    let item = if let Some(reference) = item["$ref"].as_str() {
        schema
            .pointer(
                reference
                    .strip_prefix('#')
                    .context("External item schema reference")?,
            )
            .context("Missing native item schema")?
    } else {
        item
    };
    let variant = item
        .get("oneOf")
        .or_else(|| item.get("anyOf"))
        .and_then(Value::as_array)
        .and_then(|variants| {
            variants.iter().find(|item| {
                let name = &item["properties"]["type"];
                name["const"] == "mcpToolCall"
                    || name["enum"]
                        .as_array()
                        .is_some_and(|names| names.iter().any(|name| name == "mcpToolCall"))
            })
        })
        .context("Codex omitted native mcpToolCall item evidence")?;
    for field in ["id", "server", "tool", "arguments"] {
        let property = variant["properties"]
            .get(field)
            .with_context(|| format!("Codex omitted native mcpToolCall.{field}"))?;
        ensure!(
            field == "arguments"
                || accepts_wire_value(schema, property, &json!("native-identity"), 0),
            "Codex changed the native mcpToolCall.{field} identity format"
        );
    }
    Ok(())
}

fn verify_protocol(client: &Value, server: &Value, notifications: &Value) -> Result<()> {
    for (method, fields) in CLIENT_METHODS {
        check_method(client, method, fields, true)?;
    }
    for (method, fields) in SERVER_METHODS {
        check_method(server, method, fields, false)?;
    }
    check_method(
        notifications,
        "item/completed",
        &["threadId", "turnId", "item"],
        false,
    )?;
    check_mcp_item_notification(notifications)?;
    check_method(
        notifications,
        "turn/completed",
        &["threadId", "turn"],
        false,
    )?;
    check_method(
        notifications,
        "serverRequest/resolved",
        &["threadId", "requestId"],
        false,
    )
}

async fn check_protocol(request: &StartRequest, root: &Path) -> Result<()> {
    let output = root.join("schema");
    let status = tokio::time::timeout(
        Duration::from_secs(20),
        Command::new(&request.host_executable)
            .args([
                "app-server",
                "generate-json-schema",
                "--experimental",
                "--out",
            ])
            .arg(&output)
            .current_dir(root)
            .env("CODEX_HOME", &request.auth_home)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("Codex protocol capability check timed out")??;
    ensure!(
        status.status.success(),
        "Codex cannot export its required experimental app-server capabilities: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let read = |name: &str| -> Result<Value> {
        serde_json::from_slice(
            &fs::read(output.join(name)).with_context(|| format!("Codex omitted {name}"))?,
        )
        .context("Codex exported invalid protocol JSON")
    };
    verify_protocol(
        &read("ClientRequest.json")?,
        &read("ServerRequest.json")?,
        &read("ServerNotification.json")?,
    )
}

/// Qualify the concrete native protocol, without creating test workers or
/// launching test commands during every user invocation. The metadata fork in
/// stock_request separately verifies real inherited native settings.
pub async fn qualify(request: &StartRequest) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "DeLM currently supports macOS only"
    );
    let probe = Probe::new()?;
    check_protocol(request, &probe.0).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(methods: &[(&str, &[&str])]) -> Value {
        json!({"oneOf":methods.iter().map(|(method, fields)| json!({"properties":{"method":{"enum":[method]},"params":{"properties":fields.iter().map(|field| ((*field).to_owned(), json!({}))).collect::<serde_json::Map<_,_>>()}}})).collect::<Vec<_>>()})
    }

    fn notifications() -> Value {
        let mut notification = schema(&[
            ("item/completed", &["threadId", "turnId", "item"]),
            ("turn/completed", &["threadId", "turn"]),
            ("serverRequest/resolved", &["threadId", "requestId"]),
            ("item/started", &["threadId", "turnId", "item"]),
        ]);
        notification["oneOf"][3]["properties"]["params"]["properties"]["item"] = json!({
            "oneOf":[{"properties":{"type":{"enum":["mcpToolCall"]},"id":{"type":"string"},
                "server":{"type":"string"},"tool":{"type":"string"},"arguments":{}}}]
        });
        notification
    }

    #[test]
    fn native_mcp_events_require_call_identity_and_arguments() {
        let mut notification = notifications();
        check_mcp_item_notification(&notification).unwrap();
        notification["oneOf"][3]["properties"]["params"]["properties"]["item"]["oneOf"][0]["properties"]
            .as_object_mut().unwrap().remove("arguments");
        assert!(
            check_mcp_item_notification(&notification)
                .unwrap_err()
                .to_string()
                .contains("mcpToolCall.arguments")
        );
    }

    #[test]
    fn protocol_checks_required_capabilities_and_accepts_additions() {
        let mut client = schema(CLIENT_METHODS);
        let server = schema(SERVER_METHODS);
        verify_protocol(&client, &server, &notifications()).unwrap();
        client["oneOf"][0]["properties"]["params"]["properties"]["futureOptionalField"] =
            json!({"type":"string"});
        verify_protocol(&client, &server, &notifications()).unwrap();
        client["oneOf"][0]["properties"]["params"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("beforeTurnId");
        assert!(
            verify_protocol(&client, &server, &notifications())
                .unwrap_err()
                .to_string()
                .contains("thread/fork.beforeTurnId")
        );
        let mut client = schema(CLIENT_METHODS);
        client["oneOf"][3]["properties"]["params"]["required"] =
            json!(["threadId", "input", "breakingNewField"]);
        assert!(
            verify_protocol(&client, &server, &notifications())
                .unwrap_err()
                .to_string()
                .contains("breakingNewField")
        );
    }

    #[test]
    fn protocol_requires_tool_calls_and_cleanup_before_model_turns() {
        let client = schema(CLIENT_METHODS);
        assert!(verify_protocol(&client, &schema(&[]), &notifications()).is_err());
        let server = schema(SERVER_METHODS);
        assert!(
            verify_protocol(&client, &server, &schema(&[]))
                .unwrap_err()
                .to_string()
                .contains("item/completed")
        );
        let mut client = client;
        client["oneOf"].as_array_mut().unwrap().remove(7);
        assert!(
            verify_protocol(&client, &server, &notifications())
                .unwrap_err()
                .to_string()
                .contains("thread/archive")
        );
    }

    #[test]
    fn zero_argument_native_methods_accept_null_but_reject_new_required_inputs() {
        let mut schema = schema(&[("configRequirements/read", &[])]);
        schema["oneOf"][0]["properties"]["params"] = json!({"type":"null"});
        check_method(&schema, "configRequirements/read", &[], true).unwrap();
        schema["oneOf"][0]["properties"]["params"] = json!({"type":"object", "properties":{"newField":{"type":"string"}}, "required":["newField"]});
        assert!(check_method(&schema, "configRequirements/read", &[], true).is_err());
    }

    #[test]
    fn protocol_rejects_changed_types_and_nested_wire_enums() {
        let mut schema = schema(&[("turn/start", &["threadId", "input"])]);
        let properties = &mut schema["oneOf"][0]["properties"]["params"]["properties"];
        properties["threadId"] = json!({"type":["string","null"]});
        properties["input"] = json!({"type":"array","items":{"$ref":"#/definitions/Text"}});
        schema["definitions"] = json!({"Text":{"type":"object","properties":{
            "type":{"type":"string","enum":["text"]},"text":{"type":"string"},
            "text_elements":{"type":"array"}},"required":["type","text"]}});
        check_method(&schema, "turn/start", &["threadId", "input"], true).unwrap();
        schema["definitions"]["Text"]["properties"]["type"]["enum"] = json!(["renamedText"]);
        assert!(check_method(&schema, "turn/start", &["threadId", "input"], true).is_err());
        schema["definitions"]["Text"]["properties"]["type"]["enum"] = json!(["text"]);
        schema["oneOf"][0]["properties"]["params"]["properties"]["threadId"] =
            json!({"type":"integer"});
        assert!(check_method(&schema, "turn/start", &["threadId", "input"], true).is_err());
        assert!(check_method(&schema, "turn/start", &["threadId"], false).is_err());
    }
}
