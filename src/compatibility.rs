//! Capability checks for the installed Codex, independent of its release number.
//! The probe uses disposable files and never starts a model turn.
use crate::{
    protocol::StartRequest,
    workers::{RpcClient, verify_thread_response, worker_config},
};
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
        "thread/start",
        &[
            "cwd",
            "model",
            "modelProvider",
            "serviceTier",
            "permissions",
            "config",
            "developerInstructions",
            "dynamicTools",
            "ephemeral",
            "environments",
            "runtimeWorkspaceRoots",
        ],
    ),
    (
        "thread/resume",
        &[
            "threadId",
            "cwd",
            "model",
            "modelProvider",
            "serviceTier",
            "permissions",
            "config",
            "runtimeWorkspaceRoots",
        ],
    ),
    ("thread/read", &["threadId", "includeTurns"]),
    ("turn/start", &["threadId", "input"]),
    ("turn/steer", &["threadId", "expectedTurnId", "input"]),
    ("turn/interrupt", &["threadId", "turnId"]),
    ("thread/backgroundTerminals/clean", &["threadId"]),
    ("thread/archive", &["threadId"]),
    (
        "command/exec",
        &[
            "command",
            "cwd",
            "permissionProfile",
            "timeoutMs",
            "outputBytesCap",
        ],
    ),
    ("experimentalFeature/list", &["cursor", "limit"]),
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
        "ephemeral" | "includeTurns" => json!(true),
        "config" => json!({}),
        "dynamicTools" => json!(crate::board::tool_definitions()),
        "input" => json!([crate::workers::text_input("compatibility probe")]),
        "environments" => json!([{"environmentId":"local","cwd":"/private/tmp",
            "runtimeWorkspaceRoots":["/private/tmp"]}]),
        "runtimeWorkspaceRoots" => json!(["/private/tmp"]),
        "command" => json!(["/bin/sh", "-c", "true"]),
        "timeoutMs" | "outputBytesCap" | "limit" => json!(100),
        "permissions" | "permissionProfile" => json!("delm_worker_1"),
        "modelProvider" => json!("openai"),
        "serviceTier" => json!("fast"),
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

fn verify_protocol(client: &Value, server: &Value, notifications: &Value) -> Result<()> {
    for (method, fields) in CLIENT_METHODS {
        check_method(client, method, fields, true)?;
    }
    check_method(
        server,
        "item/tool/call",
        &["threadId", "turnId", "callId", "tool", "arguments"],
        false,
    )?;
    check_method(
        server,
        "item/tool/requestUserInput",
        &["threadId", "turnId", "itemId", "questions"],
        false,
    )?;
    check_method(
        notifications,
        "item/completed",
        &["threadId", "turnId", "item"],
        false,
    )?;
    check_method(
        notifications,
        "turn/completed",
        &["threadId", "turn"],
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

async fn check_clarification_capability(rpc: &RpcClient) -> Result<()> {
    let mut cursor = Value::Null;
    for _ in 0..16 {
        let page = rpc
            .request(
                "experimentalFeature/list",
                json!({"limit":100,"cursor":cursor}),
            )
            .await?;
        let features = page["data"]
            .as_array()
            .context("Codex omitted its feature inventory")?;
        if let Some(feature) = features
            .iter()
            .find(|feature| feature["name"].as_str() == Some("default_mode_request_user_input"))
        {
            ensure!(
                feature["enabled"].as_bool() == Some(true)
                    && feature["stage"].as_str() != Some("removed"),
                "Codex cannot enable clarification in Default mode; update Codex before starting DeLM"
            );
            return Ok(());
        }
        let next = page
            .get("nextCursor")
            .context("Codex omitted feature pagination")?;
        if next.is_null() {
            break;
        }
        ensure!(
            next.is_string() && next != &cursor,
            "Invalid Codex feature pagination"
        );
        cursor = next.clone();
    }
    anyhow::bail!(
        "Codex does not support clarification in Default mode; update Codex before starting DeLM"
    )
}

// Positional arguments keep even adversarial path characters out of shell code.
// Read and append probes use shell builtins only, available on every supported Mac.
const PROBE_SCRIPT: &str = r#"
set -eu
check_read() {
  if ( IFS= read -r value < "$2" ) 2>/dev/null; then actual=allow; else actual=deny; fi
  [ "$actual" = "$1" ] || { printf 'unexpected read permission: %s\n' "$2" >&2; exit 1; }
}
check_write() {
  if ( printf 'probe write\n' >> "$2" ) 2>/dev/null; then actual=allow; else actual=deny; fi
  [ "$actual" = "$1" ] || { printf 'unexpected write permission: %s\n' "$2" >&2; exit 1; }
}
check_read allow "$1/canary"
check_write allow "$1/canary"
check_read allow "$1/readonly/canary"
check_write deny "$1/readonly/canary"
check_read deny "$1/denied/canary"
check_write deny "$1/denied/canary"
shift
for path do
  check_read deny "$path"
  check_write deny "$path"
done
check_write allow "$HOME/canary"
check_write allow "$TMPDIR/canary"
printf 'delm-isolation-ok\n'
"#;

/// Recheck on each public run: wrapper scripts can keep the same hash
/// while their backing Codex binary changes. No persistent success cache is used.
pub async fn qualify(request: &StartRequest) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "DeLM compatibility qualification currently supports macOS only"
    );
    let probe = Probe::new()?;
    check_protocol(request, &probe.0).await?;
    let original = probe.0.join("original");
    let run = probe.0.join("run");
    let project = run.join("workspace/worker-1");
    let peer = run.join("workspace/worker-2");
    let baseline = run.join("workspace/baseline");
    let secret = probe.0.join("account-fixture");
    for path in [
        &original,
        &project,
        &peer,
        &baseline,
        &secret,
        &original.join("readonly"),
        &original.join("denied"),
        &project.join("readonly"),
        &project.join("denied"),
    ] {
        fs::create_dir_all(path)?;
        fs::write(path.join("canary"), "private probe\n")?;
    }
    fs::write(run.join("control-token"), "private probe\n")?;
    let mut fixture = request.clone();
    fixture.project = original.clone();
    // Exercise precisely the same translator and named profile as real workers.
    fixture.policy["file_system"] = json!({"kind":"restricted","entries":[
        {"path":{"type":"special","value":{"kind":"root"}},"access":"read"},
        {"path":{"type":"path","path":original},"access":"write"},
        {"path":{"type":"path","path":original.join("readonly")},"access":"read"},
        {"path":{"type":"path","path":original.join("denied")},"access":"deny"},
        {"path":{"type":"path","path":secret},"access":"deny"}]});
    let config = worker_config(&fixture, &run, &project, 1)?;
    let mut rpc = RpcClient::spawn(&fixture, &run).await?;
    rpc.initialize().await?;
    check_clarification_capability(&rpc).await?;
    let response = rpc.request("thread/start", json!({"cwd":project,"model":fixture.model,"modelProvider":fixture.model_provider,"serviceTier":fixture.service_tier,
        "permissions":"delm_worker_1","config":config,"dynamicTools":crate::board::tool_definitions(),"ephemeral":true})).await?;
    verify_thread_response(&fixture, &run, &project, 1, &response)?;
    rpc.shutdown(&[]).await?;
    // command/exec selects a process profile. Supply the same generated config
    // as CLI overrides, never by writing to the user's CODEX_HOME.
    let mut rpc = RpcClient::spawn_with_config(&fixture, &run, config).await?;
    rpc.initialize().await?;
    let denied = [
        original.join("canary"),
        peer.join("canary"),
        baseline.join("canary"),
        secret.join("canary"),
        run.join("control-token"),
    ];
    let mut command = vec![
        json!("/bin/sh"),
        json!("-c"),
        json!(PROBE_SCRIPT),
        json!("delm-compatibility"),
        json!(project),
    ];
    command.extend(denied.iter().map(|path| json!(path)));
    let result = rpc.request("command/exec", json!({"command":command,"cwd":project,"permissionProfile":"delm_worker_1","timeoutMs":10000,"outputBytesCap":4096})).await;
    rpc.shutdown(&[]).await?;
    let result = result?;
    ensure!(
        result["exitCode"].as_i64() == Some(0)
            && result["stdout"]
                .as_str()
                .is_some_and(|output| output.trim() == "delm-isolation-ok"),
        "Codex did not enforce private worker isolation: {}",
        result["stderr"]
    );
    for path in denied.iter().chain([
        &project.join("readonly/canary"),
        &project.join("denied/canary"),
    ]) {
        ensure!(
            fs::read(path)? == b"private probe\n",
            "Codex changed a protected compatibility fixture"
        );
    }
    for path in [
        run.join("environment/worker-1/home/canary"),
        run.join("environment/worker-1/tmp/canary"),
    ] {
        ensure!(
            fs::read(path)? == b"probe write\n",
            "Codex did not preserve the private worker environment"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(methods: &[(&str, &[&str])]) -> Value {
        json!({"oneOf":methods.iter().map(|(method, fields)| json!({"properties":{"method":{"enum":[method]},"params":{"properties":fields.iter().map(|field| ((*field).to_owned(), json!({}))).collect::<serde_json::Map<_,_>>()}}})).collect::<Vec<_>>()})
    }

    fn notifications() -> Value {
        schema(&[
            ("item/completed", &["threadId", "turnId", "item"]),
            ("turn/completed", &["threadId", "turn"]),
        ])
    }

    #[test]
    fn protocol_checks_required_capabilities_and_accepts_additions() {
        let mut client = schema(CLIENT_METHODS);
        let server = schema(&[
            (
                "item/tool/call",
                &["threadId", "turnId", "callId", "tool", "arguments"],
            ),
            (
                "item/tool/requestUserInput",
                &["threadId", "turnId", "itemId", "questions"],
            ),
        ]);
        verify_protocol(&client, &server, &notifications()).unwrap();
        client["oneOf"][0]["properties"]["params"]["properties"]["futureOptionalField"] =
            json!({"type":"string"});
        verify_protocol(&client, &server, &notifications()).unwrap();
        client["oneOf"][0]["properties"]["params"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("permissions");
        assert!(
            verify_protocol(&client, &server, &notifications())
                .unwrap_err()
                .to_string()
                .contains("thread/start.permissions")
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
        let server = schema(&[
            (
                "item/tool/call",
                &["threadId", "turnId", "callId", "tool", "arguments"],
            ),
            (
                "item/tool/requestUserInput",
                &["threadId", "turnId", "itemId", "questions"],
            ),
        ]);
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
