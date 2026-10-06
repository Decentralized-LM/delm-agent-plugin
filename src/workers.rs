use crate::protocol::StartRequest;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{Mutex, mpsc, oneshot},
};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

/// Only project/session overrides captured through the native host are reapplied.
/// Ordinary capabilities, credentials and shell configuration remain native.
pub fn stock_overrides(settings: &Value, _run_dir: &Path) -> Result<Value> {
    let overrides = settings
        .get("native_config_overrides")
        .cloned()
        .unwrap_or_else(|| json!({}));
    ensure!(
        overrides.is_object(),
        "Native configuration overrides must be an object"
    );
    Ok(overrides)
}

/// Config/read on a separate app-server describes that server, not the live
/// parent. Never turn this structural check into an assertion of full parity.
pub fn verify_stock_configuration(response: &Value, requirements: &Value) -> Result<()> {
    ensure!(
        response.get("config").is_some_and(Value::is_object),
        "Codex omitted effective configuration"
    );
    ensure!(
        requirements.get("requirements").is_some(),
        "Codex omitted managed-policy evidence"
    );
    Ok(())
}

fn toml_literal(value: &Value) -> Result<String> {
    Ok(match value {
        Value::String(_) | Value::Bool(_) | Value::Number(_) => serde_json::to_string(value)?,
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(toml_literal)
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| Ok(format!(
                    "{}={}",
                    serde_json::to_string(key)?,
                    toml_literal(value)?
                )))
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        Value::Null => bail!("Stock configuration overrides cannot contain null"),
    })
}

fn account_identity(current: &Value) -> Result<Value> {
    match current.pointer("/account/type").and_then(Value::as_str) {
        Some("chatgpt") => {
            ensure!(
                current
                    .pointer("/workspaceRouting/chatgptAccountId")
                    .and_then(Value::as_str)
                    .is_some(),
                "Codex did not identify the native ChatGPT workspace"
            );
            Ok(
                json!({"type":"chatgpt", "email":current["account"]["email"],
                "workspace_id":current["workspaceRouting"]["chatgptAccountId"]}),
            )
        }
        Some("apiKey") => Ok(json!({"type":"api_key","resume_supported":false})),
        _ => bail!("Sign in to a supported native Codex account before starting DeLM"),
    }
}

struct ProbeDirectory(PathBuf);
impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        // This directory is created exclusively for metadata-only native probes.
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn metadata_thread_request(
    project: &Path,
    overrides: Value,
    parent: Option<&str>,
    model: Option<String>,
    effort: Option<String>,
) -> (&'static str, Value) {
    let mut params = json!({"cwd":project,"config":overrides,"ephemeral":true});
    let method = if let Some(parent) = parent {
        params["threadId"] = json!(parent);
        params["excludeTurns"] = json!(true);
        // Ephemeral forks do not inherit a goal. Native Codex rejects combining
        // them with deferGoalContinuation, which is only for persistent forks.
        "thread/fork"
    } else {
        "thread/start"
    };
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    if let Some(effort) = effort {
        params["config"]["model_reasoning_effort"] = json!(effort);
    }
    (method, params)
}

/// Read current native hook trust without opening a thread or generating tokens.
/// A fresh listing does not prove what the parent session already loaded, so it
/// supplements the invocation handshake and monitoring lease, never replaces them.
pub async fn verify_lifecycle_hooks(
    request: &StartRequest,
    binding: &crate::lifecycle::Binding,
) -> Result<()> {
    if let Some(listing) = request.auth_settings.get("startup_hook_listing") {
        return crate::lifecycle::validate_hook_listing(listing, &binding.executable);
    }
    let path = std::env::temp_dir().join(format!("delm-hook-metadata-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    let probe = ProbeDirectory(path.canonicalize()?);
    let config = stock_overrides(&request.auth_settings, &probe.0)?;
    let mut rpc = RpcClient::spawn_with_config(request, &probe.0, config).await?;
    let verified = async {
        rpc.initialize().await?;
        let listing = rpc
            .request("hooks/list", json!({"cwds":[request.project]}))
            .await?;
        crate::lifecycle::validate_hook_listing(&listing, &binding.executable)
    }
    .await;
    let shutdown = rpc.shutdown(&[]).await;
    verified?;
    shutdown
}

/// Resolve a manually requested run through the installed stock CLI.
/// Reads native metadata and qualifies isolation without generating a model turn.
pub async fn stock_request(
    project: PathBuf,
    task: String,
    context: String,
    model: Option<String>,
    effort: Option<String>,
    seconds: u64,
) -> Result<StartRequest> {
    stock_request_with_parent_turn(project, task, context, model, effort, seconds, None).await
}

pub async fn stock_request_with_parent_turn(
    project: PathBuf,
    task: String,
    context: String,
    model: Option<String>,
    effort: Option<String>,
    seconds: u64,
    parent_turn: Option<String>,
) -> Result<StartRequest> {
    let project = project
        .canonicalize()
        .context("Select an existing project directory")?;
    ensure!(project.is_dir(), "Select a project directory");
    let host_executable = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("codex"))
        .find(|path| {
            fs::metadata(path).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
        .context("Stock codex was not found on PATH")?
        .canonicalize()?;
    ensure!(
        !host_executable.to_string_lossy().contains("codex-delm"),
        "DeLM requires your stock codex installation, not codex-delm"
    );
    let version = crate::compatibility::host_version(&host_executable).await?;
    let auth_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".codex")
        })
        .canonicalize()
        .context("Native CODEX_HOME is unavailable; sign in with stock Codex first")?;
    let probe_path =
        std::env::temp_dir().join(format!("delm-stock-metadata-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&probe_path)?;
    fs::set_permissions(&probe_path, fs::Permissions::from_mode(0o700))?;
    let probe = ProbeDirectory(probe_path.canonicalize()?);
    let parent = std::env::var("CODEX_THREAD_ID")
        .ok()
        .filter(|id| !id.is_empty());
    let mut request = StartRequest {
        worker_count: crate::config::DEFAULT_WORKER_COUNT,
        project: project.clone(),
        task,
        context,
        attachments: Vec::new(),
        model: String::new(),
        model_provider: "openai".into(),
        reasoning_effort: None,
        service_tier: None,
        auth_home,
        auth_settings: json!({"parent_thread_id":parent,"host_version":version}),
        host_executable,
        seconds,
        policy: json!({}),
    };
    let mut rpc = RpcClient::spawn(&request, &probe.0).await?;
    rpc.initialize().await?;
    let (account, project_config, requirements, hooks) = tokio::try_join!(
        rpc.request("account/read", json!({"refreshToken":false})),
        rpc.request("config/read", json!({"includeLayers":true,"cwd":project})),
        rpc.request("configRequirements/read", Value::Null),
        rpc.request("hooks/list", json!({"cwds":[project]}))
    )?;
    // Validate trust from this already-running metadata host, avoiding a second
    // host and another round of native integration initialization at launch.
    request.auth_settings["startup_hook_listing"] = hooks;
    verify_stock_configuration(&project_config, &requirements)?;
    if !account["account"].is_null() {
        request.auth_settings["account_identity"] = account_identity(&account)?;
    }
    if let Some(turn) = parent_turn {
        let parent = parent
            .as_deref()
            .context("A captured invocation must identify its parent Codex thread")?;
        let inputs = read_invocation_inputs(&rpc, parent, &turn).await?;
        request.auth_settings["parent_turn_id"] = json!(turn);
        request.auth_settings["invocation_inputs"] = json!(inputs);
    }
    let cfg = &project_config["config"];
    let mut project_overrides = json!({});
    if let Some(layers) = project_config["layers"].as_array() {
        for layer in layers {
            if layer["name"]["type"] == "project" && layer["disabledReason"].is_null() {
                for key in layer["config"]
                    .as_object()
                    .context("Invalid native project config layer")?
                    .keys()
                {
                    if let Some(value) = cfg.get(key).filter(|value| !value.is_null()) {
                        project_overrides[key] = without_nulls(value);
                    }
                }
            }
        }
    }
    request.auth_settings["native_config_overrides"] = project_overrides.clone();
    request.auth_settings["configured_mcp_server_names"] = json!(
        cfg["mcp_servers"]
            .as_object()
            .map(|servers| servers.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default()
    );
    if let Some(developer) = cfg["developer_instructions"].as_str() {
        request.auth_settings["saved_developer_instructions"] = json!(developer);
    }
    request.auth_settings["model_selection_source"] = json!(if model.is_some() {
        "explicit"
    } else if parent.is_some() {
        "native-parent-fork"
    } else {
        "native-saved-project-config"
    });
    request.auth_settings["effort_selection_source"] = json!(if effort.is_some() {
        "explicit"
    } else if parent.is_some() {
        "native-parent-fork"
    } else {
        "native-saved-project-config"
    });
    let (method, fork_params) = metadata_thread_request(
        &project,
        project_overrides,
        parent.as_deref(),
        model,
        effort,
    );
    let inherited = rpc
        .request(method, fork_params)
        .await
        .context("Codex could not resolve native session settings without starting a model turn")?;
    request.model = inherited["model"]
        .as_str()
        .context("Codex omitted inherited model")?
        .into();
    request.model_provider = inherited["modelProvider"]
        .as_str()
        .context("Codex omitted inherited provider")?
        .into();
    request.reasoning_effort = inherited["reasoningEffort"].as_str().map(str::to_owned);
    request.service_tier = inherited["serviceTier"].as_str().map(str::to_owned);
    request.policy = native_policy(&inherited, cfg, &project)?;
    let mut native_settings = json!({});
    for field in [
        "approvalPolicy",
        "approvalsReviewer",
        "activePermissionProfile",
        "sandbox",
        "disabledPluginIds",
        "instructionSources",
    ] {
        if let Some(value) = inherited.get(field) {
            native_settings[field] = value.clone();
        }
    }
    request.auth_settings["native_thread_settings"] = native_settings;
    let skills = rpc
        .request("skills/list", json!({"cwds":[project],"forceReload":true}))
        .await?;
    request.auth_settings["skills_manifest"] = skill_manifest(&skills)?;
    let mut roots = std::collections::BTreeSet::new();
    for skill in request.auth_settings["skills_manifest"].as_array().unwrap() {
        if skill["scope"] == "repo" {
            let path = Path::new(skill["path"].as_str().context("Missing skill path")?);
            if let Some(root) = path.parent().and_then(Path::parent) {
                roots.insert(root.to_path_buf());
            }
        }
    }
    request.auth_settings["project_skill_roots"] = json!(roots);
    let fork_id = inherited
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .context("Codex omitted fork identity")?;
    request.auth_settings["mcp_manifest"] = mcp_manifest(&rpc, fork_id).await?;
    request.auth_settings["capability_report"] = json!({
        "source":if parent.is_some() { "native-parent-fork-and-saved-project-config" } else { "standalone-native-saved-project-config" },
        "exact_live_session_parity":false,
        "preserved":["saved_model_settings","native_permission_profile","native_auth_home","inherited_process_environment","saved_skills_plugins_hooks_and_mcp_configuration"],
        "unverified":if parent.is_some() { json!(["parent_process_cli_configuration_overrides","live_parent_tool_connections","live_parent_instruction_provider"]) } else { json!([]) },
        "saved_configuration_sha256":digest_json(cfg)?,
        "skills":request.auth_settings["skills_manifest"],
        "mcp_servers":request.auth_settings["mcp_manifest"],
        "note":if parent.is_some() { "This host does not export the active parent process configuration. Saved native configuration is preserved; exact live-session parity is not established." } else { "Standalone invocation uses the native saved project configuration; there is no live parent session to compare." }
    });
    // The metadata fork never receives turn/start and cannot spend model tokens.
    rpc.request("thread/unsubscribe", json!({"threadId":fork_id}))
        .await?;
    rpc.shutdown(&[]).await?;
    crate::compatibility::qualify(&request).await.with_context(|| {
        format!("Installed {version} did not pass DeLM compatibility checks. No model turn was started. Update Codex or DeLM and retry; your Codex installation was not changed")
    })?;
    Ok(request)
}

/// Wait only for the host to persist the submitted turn. Hooks can run before
/// this write; native history is the source of truth for images and mentions.
pub async fn read_invocation_inputs(
    rpc: &RpcClient,
    parent: &str,
    turn_id: &str,
) -> Result<Vec<Value>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let history = rpc
            .request(
                "thread/read",
                json!({"threadId":parent,"includeTurns":true}),
            )
            .await?;
        if let Some(inputs) = invocation_inputs(&history, turn_id)? {
            return Ok(inputs);
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "Codex has not made this invocation's complete native inputs available. No worker was started; retry DeLM after the submitted turn appears in the session"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub fn invocation_inputs(history: &Value, turn_id: &str) -> Result<Option<Vec<Value>>> {
    let turns = history
        .pointer("/thread/turns")
        .and_then(Value::as_array)
        .context("Codex omitted native turn history")?;
    let Some(turn) = turns
        .iter()
        .find(|turn| turn["id"].as_str() == Some(turn_id))
    else {
        return Ok(None);
    };
    ensure!(
        turn["itemsView"].is_null() || turn["itemsView"] == "full",
        "Codex returned summarized invocation inputs; full native user content is required"
    );
    let items = turn["items"]
        .as_array()
        .context("Codex omitted native turn items")?;
    let mut result = Vec::new();
    for item in items.iter().filter(|item| item["type"] == "userMessage") {
        result.extend(
            item["content"]
                .as_array()
                .context("Codex omitted native user input content")?
                .iter()
                .cloned(),
        );
    }
    Ok((!result.is_empty()).then_some(result))
}

fn without_nulls(value: &Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key.clone(), without_nulls(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(without_nulls).collect()),
        _ => value.clone(),
    }
}

fn digest_json(value: &Value) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

/// Public, non-secret evidence. Contents are hashed so matching names alone do
/// not masquerade as matching skill instructions.
pub fn skill_manifest(listing: &Value) -> Result<Value> {
    use sha2::{Digest, Sha256};
    let entries = listing["data"]
        .as_array()
        .context("Codex omitted the skill inventory")?;
    let mut manifest = Vec::new();
    for entry in entries {
        for skill in entry["skills"]
            .as_array()
            .context("Codex omitted skill entries")?
        {
            let path = skill["path"]
                .as_str()
                .context("Codex omitted a skill path")?;
            let enabled = skill["enabled"]
                .as_bool()
                .context("Codex omitted a skill enablement state")?;
            let content_hash = match fs::read(path) {
                Ok(bytes) => Some(format!("{:x}", Sha256::digest(bytes))),
                Err(_) if !enabled => None,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("Enabled skill {} is not readable at {path}", skill["name"])
                    });
                }
            };
            manifest.push(json!({"name":skill["name"],"path":path,"scope":skill["scope"],"enabled":enabled,
                "plugin_id":skill["pluginId"],"instructions_sha256":content_hash,"dependencies_sha256":digest_json(&skill["dependencies"])?}));
        }
    }
    manifest.sort_by_key(|skill| {
        (
            skill["name"].as_str().unwrap_or("").to_owned(),
            skill["path"].as_str().unwrap_or("").to_owned(),
        )
    });
    Ok(json!(manifest))
}

pub async fn mcp_manifest(rpc: &RpcClient, thread: &str) -> Result<Value> {
    let mut cursor = Value::Null;
    let mut result = Vec::new();
    for _ in 0..64 {
        let page = rpc
            .request(
                "mcpServerStatus/list",
                json!({"threadId":thread,"detail":"toolsAndAuthOnly","limit":100,"cursor":cursor}),
            )
            .await?;
        for server in page["data"]
            .as_array()
            .context("Codex omitted MCP inventory")?
        {
            let name = server["name"]
                .as_str()
                .context("Codex omitted MCP server name")?;
            result.push(json!({"name":name,"plugin_id":server["pluginId"],"auth_status":server["authStatus"],
                "tools_sha256":digest_json(&server["tools"])?,"tool_count":server["tools"].as_object().map_or(0, serde_json::Map::len),
                "discovery_failed":!server["toolsError"].is_null()}));
        }
        let next = page
            .get("nextCursor")
            .context("Codex omitted MCP inventory pagination")?;
        if next.is_null() {
            result.sort_by_key(|entry| entry["name"].as_str().unwrap_or("").to_owned());
            return Ok(json!(result));
        }
        ensure!(
            next.is_string() && next != &cursor,
            "Invalid MCP pagination"
        );
        cursor = next.clone();
    }
    bail!("MCP inventory exceeded pagination limit")
}

/// Make original project skills discoverable when an ignored local skill was
/// deliberately absent from the code snapshot. Ordinary files remain accessed
/// through native Codex permissions, not a manufactured private HOME.
pub async fn prepare_worker_capabilities(rpc: &RpcClient, request: &StartRequest) -> Result<()> {
    if let Some(roots) = request.auth_settings["project_skill_roots"]
        .as_array()
        .filter(|roots| !roots.is_empty())
    {
        rpc.request("skills/extraRoots/set", json!({"extraRoots":roots}))
            .await?;
    }
    Ok(())
}

pub async fn verify_worker_capabilities(
    rpc: &RpcClient,
    request: &StartRequest,
    project: &Path,
    thread: &str,
    worker: usize,
) -> Result<Value> {
    crate::config::validate_worker_count(request.worker_count)?;
    ensure!(
        (1..=request.worker_count).contains(&worker),
        "Worker {worker} is not a member of this DeLM run"
    );
    let coordination = format!("delm_coordination_{worker}");
    let (skills, tools) = tokio::try_join!(
        rpc.request("skills/list", json!({"cwds":[project],"forceReload":true})),
        mcp_manifest(rpc, thread)
    )?;
    let skills = skill_manifest(&skills)?;
    compare_capability_manifests(
        &request.auth_settings["skills_manifest"],
        &skills,
        &request.auth_settings["mcp_manifest"],
        &tools,
        Some(&coordination),
    )?;
    verify_skill_resources(&request.auth_settings["skills_manifest"], &skills)?;
    let compared = request.auth_settings["skills_manifest"].is_array()
        && request.auth_settings["mcp_manifest"].is_array();
    Ok(
        json!({"skills":skills,"mcp_servers":tools,"matches_saved_configuration":compared,"exact_live_session_parity":false}),
    )
}

pub fn compare_capability_manifests(
    expected_skills: &Value,
    actual_skills: &Value,
    expected_tools: &Value,
    actual_tools: &Value,
    allowed_coordination: Option<&str>,
) -> Result<()> {
    if let Some(expected) = expected_skills.as_array() {
        let actual = actual_skills
            .as_array()
            .context("Worker omitted skill manifest")?;
        for skill in expected.iter().filter(|skill| skill["enabled"] == true) {
            ensure!(
                actual.iter().any(|candidate| [
                    "name",
                    "enabled",
                    "plugin_id",
                    "instructions_sha256",
                    "dependencies_sha256"
                ]
                .iter()
                .all(|key| candidate[*key] == skill[*key])),
                "Worker is missing or changed the enabled skill {}. Its instructions and dependencies must match before DeLM can start",
                skill["name"]
            );
        }
        for skill in actual.iter().filter(|skill| skill["enabled"] == true) {
            ensure!(
                expected.iter().any(|candidate| [
                    "name",
                    "enabled",
                    "plugin_id",
                    "instructions_sha256",
                    "dependencies_sha256"
                ]
                .iter()
                .all(|key| candidate[*key] == skill[*key])),
                "Worker unexpectedly enabled a different skill {}. Its setup must match the source session",
                skill["name"]
            );
        }
    }
    if let Some(expected) = expected_tools.as_array() {
        let actual = actual_tools
            .as_array()
            .context("Worker omitted MCP manifest")?;
        for server in expected {
            ensure!(
                actual.iter().any(|candidate| [
                    "name",
                    "plugin_id",
                    "auth_status",
                    "tools_sha256",
                    "discovery_failed"
                ]
                .iter()
                .all(|key| candidate[*key] == server[*key])),
                "Worker MCP tools differ for {}. Restore that integration before starting DeLM; it was not disabled",
                server["name"]
            );
        }
        for server in actual.iter().filter(|server| {
            server["name"]
                .as_str()
                .is_none_or(|name| Some(name) != allowed_coordination)
        }) {
            ensure!(
                expected.iter().any(|candidate| [
                    "name",
                    "plugin_id",
                    "auth_status",
                    "tools_sha256",
                    "discovery_failed"
                ]
                .iter()
                .all(|key| candidate[*key] == server[*key])),
                "Worker unexpectedly changed its MCP integration {}",
                server["name"]
            );
        }
    }
    Ok(())
}

fn verify_skill_resources(expected: &Value, actual: &Value) -> Result<()> {
    let Some(expected) = expected.as_array() else {
        return Ok(());
    };
    let actual = actual.as_array().context("Worker omitted skill evidence")?;
    for skill in expected.iter().filter(|skill| skill["enabled"] == true) {
        let source = Path::new(
            skill["path"]
                .as_str()
                .context("Missing source skill path")?,
        );
        let candidates = actual
            .iter()
            .filter(|candidate| {
                candidate["enabled"] == true
                    && candidate["name"] == skill["name"]
                    && candidate["instructions_sha256"] == skill["instructions_sha256"]
            })
            .collect::<Vec<_>>();
        let mut matched = false;
        for candidate in candidates {
            let selected = Path::new(
                candidate["path"]
                    .as_str()
                    .context("Missing worker skill path")?,
            );
            // Global/plugin skills retain the same original resources. Compare
            // complete bundles only when workspace rebinding changed the path.
            if source.canonicalize()? == selected.canonicalize()? {
                matched = true;
                break;
            }
            if skill_bundle(source.parent().context("Invalid source skill path")?)?
                == skill_bundle(selected.parent().context("Invalid worker skill path")?)?
            {
                matched = true;
                break;
            }
        }
        ensure!(
            matched,
            "Worker skill {} is missing supporting files from its original bundle",
            skill["name"]
        );
    }
    Ok(())
}

fn skill_bundle(root: &Path) -> Result<std::collections::BTreeMap<PathBuf, String>> {
    use sha2::{Digest, Sha256};
    fn visit(
        root: &Path,
        path: &Path,
        files: &mut std::collections::BTreeMap<PathBuf, String>,
    ) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                visit(root, &entry?.path(), files)?;
            }
        } else if metadata.is_file() {
            let mut file = fs::File::open(path)?;
            let mut digest = Sha256::new();
            std::io::copy(&mut file, &mut digest)?;
            files.insert(
                path.strip_prefix(root)?.to_path_buf(),
                format!("{:x}", digest.finalize()),
            );
        } else if metadata.file_type().is_symlink() {
            fs::metadata(path).with_context(|| {
                format!("Skill resource link is unavailable: {}", path.display())
            })?;
            files.insert(
                path.strip_prefix(root)?.to_path_buf(),
                format!("link:{}", fs::read_link(path)?.to_string_lossy()),
            );
        } else {
            bail!(
                "Unsupported special file in skill bundle: {}",
                path.display()
            );
        }
        Ok(())
    }
    let mut files = std::collections::BTreeMap::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn native_policy(native: &Value, config: &Value, project: &Path) -> Result<Value> {
    let sandbox = &native["sandbox"];
    let kind = sandbox["type"]
        .as_str()
        .context("Codex omitted inherited sandbox")?;
    let network = kind == "dangerFullAccess" || sandbox["networkAccess"].as_bool() == Some(true);
    let mut entries =
        vec![json!({"path":{"type":"special","value":{"kind":"root"}},"access":"read"})];
    if kind == "workspaceWrite" {
        entries.push(json!({"path":{"type":"path","path":project},"access":"write"}));
        if let Some(roots) = sandbox["writableRoots"].as_array() {
            entries.extend(
                roots
                    .iter()
                    .map(|path| json!({"path":{"type":"path","path":path},"access":"write"})),
            );
        }
    }
    if let Some(id) = native
        .pointer("/activePermissionProfile/id")
        .and_then(Value::as_str)
        && let Some(filesystem) = config["permissions"][id]["filesystem"].as_object()
    {
        entries.clear();
        for (path, access) in filesystem {
            let source = match path.as_str() {
                ":root" | "/" => json!({"type":"special","value":{"kind":"root"}}),
                ":project_roots" => json!({"type":"special","value":{"kind":"project_roots"}}),
                ":minimal" => json!({"type":"special","value":{"kind":"minimal"}}),
                ":tmpdir" => json!({"type":"special","value":{"kind":"tmpdir"}}),
                ":slash_tmp" => json!({"type":"special","value":{"kind":"slash_tmp"}}),
                path if path.contains(['*', '?', '[', ']', '{', '}']) => {
                    json!({"type":"glob_pattern","pattern":path})
                }
                path if path.starts_with('/') => json!({"type":"path","path":path}),
                _ => bail!(
                    "Native permission rule {path} needs a board-transfer authorization adapter"
                ),
            };
            entries.push(json!({"path":source,"access":access}));
        }
    }
    Ok(
        json!({"approval_policy":native["approvalPolicy"],"sandbox":sandbox,
        "file_system":if kind == "dangerFullAccess" { json!({"kind":"unrestricted"}) } else { json!({"kind":"restricted","entries":entries}) },
        "network":if network { "enabled" } else { "restricted" },"network_proxy_active":false}),
    )
}

pub struct RpcClient {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    pending: Pending,
    next_id: AtomicU64,
    ingress_sequence: Arc<AtomicU64>,
    pub events: mpsc::UnboundedReceiver<Value>,
    child: Child,
    pub pid: u32,
    pub launch_dir: PathBuf,
    reader: tokio::task::JoinHandle<()>,
}

impl RpcClient {
    pub fn received_sequence(&self) -> u64 {
        self.ingress_sequence.load(Ordering::SeqCst)
    }

    pub async fn spawn(request: &StartRequest, run_dir: &Path) -> Result<Self> {
        let run_dir = run_dir.canonicalize()?;
        Self::spawn_with_config(
            request,
            &run_dir,
            stock_overrides(&request.auth_settings, &run_dir)?,
        )
        .await
    }

    pub(crate) async fn spawn_with_config(
        request: &StartRequest,
        run_dir: &Path,
        config: Value,
    ) -> Result<Self> {
        ensure!(
            request.auth_settings.is_object(),
            "Native account settings must be an object"
        );

        let run_dir = run_dir.canonicalize()?;
        let launches = run_dir.join("launches");
        fs::create_dir_all(&launches)?;
        let launch_dir = launches.join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&launch_dir)?;
        fs::set_permissions(&launch_dir, fs::Permissions::from_mode(0o700))?;
        let stderr = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(launch_dir.join("worker-host.log"))?;
        let mut command = Command::new(&request.host_executable);
        command.arg("app-server").arg("--listen").arg("stdio://");
        for (key, value) in config.as_object().context("Invalid worker overrides")? {
            command
                .arg("--config")
                .arg(format!("{key}={}", toml_literal(value)?));
        }
        command
            .current_dir(&run_dir)
            .env("CODEX_HOME", &request.auth_home)
            .env("DELM_WORKER_SESSION", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .kill_on_drop(true);
        // A dedicated group confines cancellation to this run. The independent
        // watchdog also observes the runtime lifetime and fences orphaned hosts.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .context("Could not start the installed Codex worker host")?;
        let pid = child.id().context("Worker host has no PID")?;
        let stdin = Arc::new(Mutex::new(
            child.stdin.take().context("Missing worker input")?,
        ));
        let stdout = child.stdout.take().context("Missing worker output")?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();
        let (tx, events) = mpsc::unbounded_channel();
        let ingress_sequence = Arc::new(AtomicU64::new(0));
        let received = ingress_sequence.clone();
        let reader = tokio::spawn(async move {
            let mut input = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                let n = match (&mut input)
                    .take(32 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut line)
                    .await
                {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if n == 0 {
                    break;
                }
                if line.len() > 32 * 1024 * 1024 {
                    let _ = tx.send(json!({"method":"delm/transportError","params":{"message":"Worker message exceeded the transport limit"}}));
                    break;
                }
                let mut value: Value = match serde_json::from_slice(&line) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = tx.send(json!({"method":"delm/transportError","params":{"message":"Invalid worker protocol output"}}));
                        break;
                    }
                };
                let sequence = received.fetch_add(1, Ordering::SeqCst) + 1;
                if value.get("method").is_none() {
                    if let Some(id) = value.get("id").and_then(Value::as_u64)
                        && let Some(sender) = pending_reader.lock().await.remove(&id)
                    {
                        let result = if let Some(error) = value.get("error") {
                            Err(anyhow::anyhow!("Codex request failed: {error}"))
                        } else {
                            Ok(value.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                } else {
                    value["_delm_received_sequence"] = json!(sequence);
                    if tx.send(value).is_err() {
                        break;
                    }
                }
            }
            let _ = tx.send(json!({"method":"delm/transportClosed"}));
            for (_, sender) in pending_reader.lock().await.drain() {
                let _ = sender.send(Err(anyhow::anyhow!("Worker connection closed")));
            }
        });
        let client = Self {
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            ingress_sequence,
            events,
            child,
            pid,
            launch_dir,
            reader,
        };
        Ok(client)
    }

    pub async fn initialize(&self) -> Result<()> {
        self.request("initialize", json!({"clientInfo":{"name":"delm","title":"DeLM","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
        self.notify("initialized", json!({})).await
    }

    pub async fn verify_account(&self, request: &StartRequest) -> Result<()> {
        if let Some(expected) = request.auth_settings.get("account_identity") {
            let current = self
                .request("account/read", json!({"refreshToken":false}))
                .await?;
            ensure!(
                &account_identity(&current)? == expected,
                "The native account changed before the workers started. No model turn was started; reopen DeLM with the current account."
            );
        }
        let requirements = self.request("configRequirements/read", Value::Null).await?;
        let config = self
            .request(
                "config/read",
                json!({"includeLayers":true,"cwd":request.project}),
            )
            .await?;
        verify_stock_configuration(&config, &requirements)?;
        Ok(())
    }

    async fn write(&self, value: Value) -> Result<()> {
        let mut encoded = serde_json::to_vec(&value)?;
        encoded.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&encoded).await?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn request(&self, method: &str, mut params: Value) -> Result<Value> {
        if method == "thread/start" {
            let cwd = params["cwd"]
                .as_str()
                .context("Worker thread needs a working directory")?
                .to_owned();
            params["environments"] =
                json!([{"environmentId":"local","cwd":cwd,"runtimeWorkspaceRoots":[cwd]}]);
            params["runtimeWorkspaceRoots"] = json!([cwd]);
        }
        self.request_raw(method, params).await
    }

    async fn request_raw(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if let Err(e) = self
            .write(json!({"id":id,"method":method,"params":params}))
            .await
        {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(Duration::from_secs(45), rx).await {
            Ok(Ok(result)) => result,
            _ => {
                self.pending.lock().await.remove(&id);
                bail!("Codex did not acknowledge {method}; no replacement request was sent")
            }
        }
    }
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write(json!({"method":method,"params":params})).await
    }
    pub async fn respond(&self, id: Value, result: Value) -> Result<()> {
        self.write(json!({"id":id,"result":result})).await
    }
    pub async fn reject(&self, id: Value, message: &str) -> Result<()> {
        self.write(json!({"id":id,"error":{"code":-32602,"message":message}}))
            .await
    }

    pub async fn shutdown(&mut self, threads: &[(String, Option<String>)]) -> Result<()> {
        ensure!(
            threads.len() <= crate::config::MAX_WORKER_COUNT,
            "Worker thread count exceeds the supported limit"
        );
        let native = tokio::time::timeout(Duration::from_secs(8), async {
            // Dispatch every interrupt concurrently before cleanup. A slow
            // worker must not extend another worker's model work.
            let slots: Vec<_> = (0..crate::config::MAX_WORKER_COUNT)
                .map(|index| threads.get(index))
                .collect();
            let (a, b, c, d) = tokio::join!(
                self.interrupt_worker(slots[0]),
                self.interrupt_worker(slots[1]),
                self.interrupt_worker(slots[2]),
                self.interrupt_worker(slots[3])
            );
            let (e, f, g, h) = tokio::join!(
                self.clean_worker(slots[0]),
                self.clean_worker(slots[1]),
                self.clean_worker(slots[2]),
                self.clean_worker(slots[3])
            );
            a && b && c && d && e && f && g && h
        })
        .await;
        let clean = matches!(native, Ok(true));
        self.stdin.lock().await.shutdown().await.ok();
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_secs(3), self.child.wait())
            .await
            .is_err()
        {
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGKILL);
            }
            self.child.wait().await?;
        }
        self.reader.abort();
        ensure!(
            clean,
            "Native worker shutdown was not fully acknowledged; preserve all worker projects"
        );
        Ok(())
    }

    async fn clean_worker(&self, worker: Option<&(String, Option<String>)>) -> bool {
        let Some((thread, _)) = worker.filter(|(thread, _)| !thread.is_empty()) else {
            return true;
        };
        let mut clean = true;
        for method in ["thread/backgroundTerminals/clean", "thread/archive"] {
            clean &= matches!(
                tokio::time::timeout(
                    Duration::from_secs(2),
                    self.request(method, json!({"threadId":thread}))
                )
                .await,
                Ok(Ok(_))
            );
        }
        clean
    }

    async fn interrupt_worker(&self, worker: Option<&(String, Option<String>)>) -> bool {
        let Some((thread, Some(turn))) = worker else {
            return true;
        };
        if thread.is_empty() {
            return true;
        }
        matches!(
            tokio::time::timeout(
                Duration::from_secs(2),
                self.request("turn/interrupt", json!({"threadId":thread,"turnId":turn}))
            )
            .await,
            Ok(Ok(_))
        )
    }
}
impl Drop for RpcClient {
    fn drop(&mut self) {
        // The unreaped child identity is still owned here. Never target a shared
        // Codex daemon or a process selected by its command name.
        if self.child.id().is_some() {
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGKILL);
            }
        }
        self.reader.abort();
    }
}

pub fn text_input(text: &str) -> Value {
    json!({"type":"text","text":text,"text_elements":[]})
}

/// Board-transfer policy and the small set of DeLM session additions. The
/// profile is for privileged board I/O only; native forks inherit their own
/// permissions. Do not select this profile as the worker's Codex sandbox.
pub fn worker_config(
    request: &StartRequest,
    run_dir: &Path,
    project: &Path,
    worker: usize,
) -> Result<Value> {
    crate::config::validate_worker_count(request.worker_count)?;
    ensure!(
        (1..=request.worker_count).contains(&worker),
        "Worker identity is outside this run"
    );
    let original = request.project.canonicalize()?;
    let run_dir = run_dir.canonicalize()?;
    let project = project.canonicalize()?;
    ensure!(
        project.starts_with(&run_dir)
            && !project.starts_with(&original)
            && !original.starts_with(&project),
        "Worker project must be inside its run and separate from the original"
    );
    let native = request
        .policy
        .get("file_system")
        .context("Host did not supply its effective filesystem policy")?;
    let unrestricted = native["kind"].as_str() == Some("unrestricted");
    ensure!(
        unrestricted || native["kind"].as_str() == Some("restricted"),
        "The host's external filesystem policy needs a native transfer authorization adapter"
    );
    let mut filesystem = serde_json::Map::new();
    let mut project_access = if unrestricted { "write" } else { "deny" }.to_owned();
    let mut rules = Vec::new();
    if let Some(entries) = native["entries"].as_array() {
        for entry in entries {
            let access = entry["access"]
                .as_str()
                .context("Invalid inherited filesystem access")?;
            ensure!(
                ["read", "write", "deny"].contains(&access),
                "Unknown inherited filesystem access"
            );
            let source = &entry["path"];
            match source["type"].as_str() {
                Some("path") => {
                    let scope = canonical_permission_path(Path::new(
                        source["path"].as_str().context("Invalid inherited path")?,
                    ))?;
                    ensure!(
                        scope.is_absolute(),
                        "Inherited permission paths must be absolute"
                    );
                    rules.push((scope, access.to_owned()));
                }
                Some("special") => match source["value"]["kind"].as_str() {
                    Some("root") => project_access = access.to_owned(),
                    Some("project_roots") => {
                        let suffix = source["value"]["subpath"].as_str().unwrap_or("");
                        ensure!(
                            !Path::new(suffix).is_absolute()
                                && !Path::new(suffix)
                                    .components()
                                    .any(|part| matches!(part, std::path::Component::ParentDir)),
                            "Invalid inherited project subpath"
                        );
                        rules.push((original.join(suffix), access.to_owned()));
                    }
                    Some("minimal" | "tmpdir" | "slash_tmp") => {}
                    _ => {
                        bail!("Unknown inherited special path needs native transfer authorization")
                    }
                },
                Some("glob_pattern") => bail!(
                    "Inherited filesystem globs need native board-transfer authorization; their restrictions were not removed"
                ),
                _ => bail!("Unknown inherited filesystem policy shape"),
            }
        }
    }
    rules.sort_by_key(|(path, access)| {
        (
            path.components().count(),
            match access.as_str() {
                "deny" => 2,
                "write" => 1,
                _ => 0,
            },
        )
    });
    for (scope, access) in &rules {
        if permission_relative(&original, scope)?.is_some() {
            project_access = access.clone();
        }
    }
    filesystem.insert(
        project.to_string_lossy().into_owned(),
        json!(project_access),
    );
    for (scope, access) in rules {
        if let Some(relative) = permission_relative(&scope, &original)? {
            filesystem.insert(
                project.join(relative).to_string_lossy().into_owned(),
                json!(access),
            );
        }
    }
    ensure!(
        filesystem
            .values()
            .any(|access| access.as_str() == Some("write")),
        "The selected Codex permissions do not allow implementation in this project"
    );
    let profile = format!("delm_worker_{worker}");
    Ok(
        json!({"default_permissions":profile, "permissions":{profile.clone():{"filesystem":filesystem}}}),
    )
}

/// Completion inspection uses native read authority, not the narrower scopes
/// used for peer file exchange. Owned project/storage paths remain excluded as
/// external interpreter dependencies even under an unrestricted native profile.
pub fn result_policy(
    request: &StartRequest,
    run_dir: &Path,
) -> Result<crate::workspace::ResultPolicy> {
    let filesystem = &request.policy["file_system"];
    let mut roots = Vec::new();
    let mut denied = vec![request.project.canonicalize()?, run_dir.canonicalize()?];
    if filesystem["kind"] == "unrestricted" {
        roots.push(PathBuf::from("/"));
    } else {
        ensure!(
            filesystem["kind"] == "restricted",
            "Unknown native runtime read policy"
        );
        for entry in filesystem["entries"]
            .as_array()
            .context("Native filesystem entries are absent")?
        {
            let path = match entry["path"]["type"].as_str() {
                Some("path") => Some(PathBuf::from(
                    entry["path"]["path"]
                        .as_str()
                        .context("Missing permission path")?,
                )),
                Some("special") => match entry["path"]["value"]["kind"].as_str() {
                    Some("root") => Some(PathBuf::from("/")),
                    Some("project_roots") => Some(request.project.clone()),
                    Some("minimal" | "tmpdir" | "slash_tmp") => None,
                    _ => bail!("Unsupported native runtime read scope"),
                },
                _ => bail!("Native runtime inspection needs an adapter for this filesystem rule"),
            };
            if let Some(path) = path {
                ensure!(
                    path.is_absolute(),
                    "Runtime permission path must be absolute"
                );
                match entry["access"].as_str() {
                    Some("read" | "write") => roots.push(path),
                    Some("deny") => denied.push(path),
                    _ => bail!("Unknown native runtime access"),
                }
            }
        }
    }
    Ok(crate::workspace::ResultPolicy {
        native_python_runtime: false,
        readonly_runtime_roots: roots,
        denied_roots: denied,
    })
}

/// Build a native fork without replacing the parent's permission profile or
/// reducing its capabilities. Coordination is a native MCP extension supplied by
/// the runtime; thread/fork has no dynamicTools parameter.
pub fn worker_thread_request(
    request: &StartRequest,
    project: &Path,
    worker: usize,
    additions: Value,
    instructions: &str,
) -> Result<(&'static str, Value)> {
    crate::config::validate_worker_count(request.worker_count)?;
    ensure!(
        (1..=request.worker_count).contains(&worker),
        "Worker identity is outside this run"
    );
    let parent = request.auth_settings["parent_thread_id"].as_str();
    let mut config = stock_overrides(&request.auth_settings, project)?;
    rebase_project_paths(&mut config, &request.project, project);
    let mut additions = additions
        .as_object()
        .context("Worker additions must be an object")?
        .clone();
    if let Some(servers) = additions.get("mcp_servers").and_then(Value::as_object) {
        for name in servers.keys() {
            let configured = config["mcp_servers"].get(name).is_some()
                || request.auth_settings["configured_mcp_server_names"]
                    .as_array()
                    .is_some_and(|names| {
                        names
                            .iter()
                            .any(|candidate| candidate.as_str() == Some(name))
                    })
                || request.auth_settings["mcp_manifest"]
                    .as_array()
                    .is_some_and(|manifest| {
                        manifest
                            .iter()
                            .any(|server| server["name"].as_str() == Some(name))
                    });
            ensure!(
                !configured,
                "Your Codex setup already defines MCP server {name}. DeLM cannot replace an existing integration; choose a different name for that server before starting DeLM"
            );
        }
    }
    // These are board I/O scopes, not native permission replacements.
    additions.remove("permissions");
    additions.remove("default_permissions");
    merge_config(&mut config, &Value::Object(additions));
    let inherited_developer = request.auth_settings["saved_developer_instructions"]
        .as_str()
        .unwrap_or("");
    let developer = if inherited_developer.is_empty() {
        instructions.to_owned()
    } else {
        format!("{inherited_developer}\n\n{instructions}")
    };
    let Some(parent) = parent else {
        // The explicit host protocol can supply an independent task and native
        // policy. Public plugin invocations always use the bound parent fork.
        let mut params = json!({"cwd":project,"config":config,"model":request.model,
            "modelProvider":request.model_provider,"serviceTier":request.service_tier,
            "approvalPolicy":request.policy["approval_policy"],"developerInstructions":developer,
            "dynamicTools":crate::board::tool_definitions(),"ephemeral":false});
        if let Some(effort) = &request.reasoning_effort {
            params["config"]["model_reasoning_effort"] = json!(effort);
        }
        params["sandbox"] = json!(match request.policy["file_system"]["kind"].as_str() {
            Some("unrestricted") => "danger-full-access",
            _ => "workspace-write",
        });
        return Ok(("thread/start", params));
    };
    let mut params = json!({"threadId":parent,"cwd":project,"config":config,
        "developerInstructions":developer,"runtimeWorkspaceRoots":[project],
        "excludeTurns":true,"deferGoalContinuation":true,"ephemeral":false});
    if let Some(turn) = request.auth_settings["parent_turn_id"].as_str() {
        params["beforeTurnId"] = json!(turn);
    }
    // Explicit user choices are forwarded; otherwise native fork inheritance
    // selects the source thread's saved values, including permission settings.
    if request.auth_settings["model_selection_source"] == "explicit" {
        params["model"] = json!(request.model);
    }
    if request.auth_settings["effort_selection_source"] == "explicit" {
        params["config"]["model_reasoning_effort"] = json!(request.reasoning_effort);
    }
    Ok(("thread/fork", params))
}

fn merge_config(target: &mut Value, overlay: &Value) {
    match (target, overlay) {
        (Value::Object(target), Value::Object(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = target.get_mut(key) {
                    merge_config(existing, value);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        (target, overlay) => *target = overlay.clone(),
    }
}

fn rebase_project_paths(value: &mut Value, original: &Path, project: &Path) {
    match value {
        Value::String(text) => {
            if let Ok(relative) = Path::new(text).strip_prefix(original) {
                *text = project.join(relative).to_string_lossy().into_owned();
            }
        }
        Value::Array(values) => {
            for value in values {
                rebase_project_paths(value, original, project);
            }
        }
        Value::Object(values) => {
            let prior = std::mem::take(values);
            for (key, mut value) in prior {
                let key = Path::new(&key)
                    .strip_prefix(original)
                    .map(|relative| project.join(relative).to_string_lossy().into_owned())
                    .unwrap_or(key);
                rebase_project_paths(&mut value, original, project);
                values.insert(key, value);
            }
        }
        _ => {}
    }
}

// Canonicalize existing ancestors even when a denied leaf does not exist yet.
fn canonical_permission_path(path: &Path) -> Result<PathBuf> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    let mut resolved = loop {
        match ancestor.canonicalize() {
            Ok(resolved) => break resolved,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .context("permission scope has no existing ancestor")?,
                );
                ancestor = ancestor
                    .parent()
                    .context("permission scope has no parent")?;
            }
            Err(error) => return Err(error).context("resolve inherited permission scope"),
        }
    };
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

// APFS canonicalize preserves caller-supplied case. Bind source ancestry by
// physical identity so differently spelled source scopes cannot lose denials
// when mapped to the private tree. Missing suffixes are preserved verbatim.
fn permission_relative(path: &Path, root: &Path) -> Result<Option<PathBuf>> {
    use std::os::unix::fs::MetadataExt;
    let root_id = match fs::metadata(root) {
        Ok(metadata) => (metadata.dev(), metadata.ino()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(path.strip_prefix(root).ok().map(Path::to_path_buf));
        }
        Err(error) => return Err(error).context("inspect inherited permission root"),
    };
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        match fs::metadata(ancestor) {
            Ok(metadata) if (metadata.dev(), metadata.ino()) == root_id => {
                return Ok(Some(suffix.into_iter().rev().collect()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect inherited permission ancestry"),
        }
        let Some(name) = ancestor.file_name() else {
            return Ok(None);
        };
        suffix.push(name);
        let Some(parent) = ancestor.parent() else {
            return Ok(None);
        };
        ancestor = parent;
    }
}

/// Validate actual native fork settings without substituting a new approval or
/// sandbox policy. Unknown live-parent fields remain explicit in the report.
pub fn verify_thread_response(
    request: &StartRequest,
    _run_dir: &Path,
    project: &Path,
    worker: usize,
    response: &Value,
) -> Result<()> {
    crate::config::validate_worker_count(request.worker_count)?;
    ensure!(
        (1..=request.worker_count).contains(&worker),
        "Unbound worker identity"
    );
    ensure!(
        Path::new(
            response["cwd"]
                .as_str()
                .context("Codex omitted effective cwd")?
        )
        .canonicalize()?
            == project.canonicalize()?,
        "Codex changed the worker working directory"
    );
    for (field, expected) in [
        ("model", request.model.as_str()),
        ("modelProvider", request.model_provider.as_str()),
    ] {
        ensure!(
            response[field].as_str() == Some(expected),
            "Codex changed inherited {field}"
        );
    }
    if let Some(effort) = &request.reasoning_effort {
        ensure!(
            response["reasoningEffort"].as_str() == Some(effort),
            "Codex changed inherited reasoning effort"
        );
    }
    if let Some(tier) = &request.service_tier {
        ensure!(
            response["serviceTier"].as_str() == Some(tier),
            "Codex changed inherited service tier"
        );
    }
    if let Some(approval) = request.policy.get("approval_policy") {
        ensure!(
            &response["approvalPolicy"] == approval,
            "Codex changed inherited approval policy"
        );
    }
    if let Some(expected) = request.auth_settings.get("native_thread_settings") {
        for field in [
            "approvalsReviewer",
            "activePermissionProfile",
            "disabledPluginIds",
        ] {
            if let Some(value) = expected.get(field) {
                ensure!(&response[field] == value, "Codex changed inherited {field}");
            }
        }
        if let Some(sandbox) = expected.get("sandbox") {
            ensure!(
                response["sandbox"]["type"] == sandbox["type"],
                "Codex changed inherited sandbox mode"
            );
            ensure!(
                response["sandbox"]["networkAccess"] == sandbox["networkAccess"],
                "Codex changed inherited network permission"
            );
        }
    }
    ensure!(
        response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "Codex omitted native thread identity"
    );
    Ok(())
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    #[test]
    fn metadata_forks_are_ephemeral_without_goal_inheritance() {
        for parent in [None, Some("active-parent")] {
            let (method, params) = metadata_thread_request(
                Path::new("/project"),
                json!({"features":{"plugins":true}}),
                parent,
                Some("fixture".into()),
                Some("medium".into()),
            );
            assert_eq!(
                method,
                if parent.is_some() {
                    "thread/fork"
                } else {
                    "thread/start"
                }
            );
            assert_eq!(params["ephemeral"], true);
            assert!(params.get("deferGoalContinuation").is_none());
            assert_eq!(params["model"], "fixture");
            assert_eq!(params["config"]["model_reasoning_effort"], "medium");
            assert_eq!(params["config"]["features"]["plugins"], true);
            if let Some(parent) = parent {
                assert_eq!(params["threadId"], parent);
                assert_eq!(params["excludeTurns"], true);
            } else {
                assert!(params.get("threadId").is_none());
            }
        }
    }

    #[tokio::test]
    async fn metadata_fork_uses_native_compatible_flags_without_a_model_turn() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let host = root.join("codex");
        fs::write(&host, include_str!("../tests/fixtures/worker_host.py")).unwrap();
        fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(host.with_extension("json"), r#"{"mode":"metadata_only"}"#).unwrap();
        let request: StartRequest = serde_json::from_value(json!({
            "project":root,"task":"Metadata only","model":"fixture",
            "auth_home":root,"host_executable":host,"policy":{}
        }))
        .unwrap();
        let mut rpc = RpcClient::spawn(&request, &root).await.unwrap();
        rpc.initialize().await.unwrap();
        let (method, params) =
            metadata_thread_request(&root, json!({}), Some("live-parent"), None, None);
        let mut invalid = params.clone();
        invalid["deferGoalContinuation"] = json!(true);
        let error = rpc.request(method, invalid).await.unwrap_err().to_string();
        assert!(
            error.contains("cannot be combined with `ephemeral`"),
            "{error}"
        );
        let inherited = rpc.request(method, params).await.unwrap();
        assert_eq!(inherited["model"], "fixture");
        assert_eq!(inherited["thread"]["id"], "ephemeral-compatibility");
        rpc.request(
            "thread/unsubscribe",
            json!({"threadId":inherited["thread"]["id"]}),
        )
        .await
        .unwrap();
        rpc.shutdown(&[]).await.unwrap();
        let wire = fs::read_to_string(host.with_extension("jsonl")).unwrap();
        for line in wire.lines() {
            let record: Value = serde_json::from_str(line).unwrap();
            if record["direction"] == "in" {
                assert!(
                    matches!(
                        record["message"]["method"].as_str(),
                        Some("initialize" | "initialized" | "thread/fork" | "thread/unsubscribe")
                    ),
                    "metadata lookup must not resume or start a model turn: {record}"
                );
            }
        }
        assert!(
            !host.with_extension("state.json").exists(),
            "metadata must not persist a worker thread"
        );
    }
}
