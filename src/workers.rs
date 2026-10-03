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

const DISABLED_FEATURES: &[&str] = &[
    "hooks",
    "plugins",
    "multi_agent",
    "multi_agent_v2",
    "memories",
    "external_agent_memory_import",
    "recommended_plugins",
    "tool_suggest",
    "apps",
    "shell_snapshot",
    "network_proxy",
    "in_app_browser",
    "browser_use",
    "browser_use_external",
    "computer_use",
    "remote_plugin",
];

// These optional integrations also require a parent capability that remains
// explicitly disabled. A removed optional flag must not break a newer host.
const OPTIONAL_FEATURE_GUARDS: &[(&str, &str)] = &[
    ("multi_agent_v2", "multi_agent"),
    ("external_agent_memory_import", "memories"),
    ("recommended_plugins", "plugins"),
    ("tool_suggest", "plugins"),
    ("remote_plugin", "plugins"),
];

/// Session overrides only: never edit the user's native configuration or credentials.
pub fn stock_overrides(settings: &Value, run_dir: &Path) -> Result<Value> {
    let mut config = json!({
        "features": {}, "agents":{"enabled":false},
        "skills":{"include_instructions":false}, "cloud":{"skills":{"enabled":false}},
        "orchestrator":{"mcp":{"enabled":false}}, "include_apps_instructions":false,
        "memories":{"generate_memories":false,"use_memories":false},
        "notify":[], "allow_login_shell":false, "web_search":"live",
        "shell_environment_policy":{"inherit":"none","set":{}},
        "projects":{}, "mcp_servers":{}
    });
    for feature in DISABLED_FEATURES {
        config["features"][*feature] = json!(false);
    }
    config["features"]["view_image"] = json!(true);
    config["features"]["default_mode_request_user_input"] = json!(true);
    for path in [
        run_dir.to_path_buf(),
        run_dir.join("workspace"),
        run_dir.join("workspace/worker-1"),
        run_dir.join("workspace/worker-2"),
    ] {
        config["projects"][path.to_string_lossy().as_ref()] = json!({"trust_level":"untrusted"});
    }
    for name in setting_names(settings, "stock_mcp_servers")? {
        config["mcp_servers"][name] = json!({"enabled":false});
    }
    // Tables merge in Codex. Empty tables do not clear the user's configured
    // variables; blank each previously observed key and verify again at launch.
    for name in setting_names(settings, "stock_environment_keys")? {
        config["shell_environment_policy"]["set"][name] = json!("");
    }
    Ok(config)
}

fn setting_names<'a>(settings: &'a Value, key: &str) -> Result<Vec<&'a str>> {
    let Some(value) = settings.get(key) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .context("Invalid stock configuration inventory")?
        .iter()
        .map(|name| name.as_str().context("Invalid stock configuration name"))
        .collect()
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

/// Validate supported config/read and configRequirements/read responses before
/// starting threads. Managed settings win over CLI flags, so flags alone are not proof.
pub fn verify_stock_configuration(response: &Value, requirements: &Value) -> Result<()> {
    let config = response
        .get("config")
        .context("Codex omitted effective configuration")?;
    let required = requirements
        .get("requirements")
        .context("Codex omitted managed-policy evidence")?;
    for feature in DISABLED_FEATURES {
        let value = &config["features"][*feature];
        let disabled = |value: &Value| {
            value.as_bool() == Some(false)
                || value.get("enabled").and_then(Value::as_bool) == Some(false)
        };
        let guarded_absence = value.is_null()
            && OPTIONAL_FEATURE_GUARDS.iter().any(|(optional, guard)| {
                optional == feature && disabled(&config["features"][*guard])
            });
        ensure!(
            disabled(value) || guarded_absence,
            "Codex did not disable worker capability {feature}"
        );
        ensure!(
            required["featureRequirements"][*feature].as_bool() != Some(true),
            "Managed policy requires worker capability {feature}"
        );
    }
    ensure!(
        required.get("hooks").is_none_or(Value::is_null),
        "Managed hooks are unavailable in isolated DeLM workers"
    );
    ensure!(
        required.get("network").is_none_or(Value::is_null),
        "DeLM cannot preserve this managed network policy in private workers; no task was started"
    );
    for profile in ["delm_worker_1", "delm_worker_2"] {
        ensure!(
            config["permissions"].get(profile).is_none(),
            "The native configuration already defines reserved permission profile {profile}; DeLM will not merge unrelated grants into worker authority"
        );
    }
    for pointer in [
        "/agents/enabled",
        "/skills/include_instructions",
        "/cloud/skills/enabled",
        "/orchestrator/mcp/enabled",
        "/include_apps_instructions",
        "/memories/generate_memories",
        "/memories/use_memories",
        "/allow_login_shell",
    ] {
        ensure!(
            config.pointer(pointer).and_then(Value::as_bool) == Some(false),
            "Codex did not isolate worker setting {pointer}"
        );
    }
    ensure!(
        config["notify"].as_array().is_some_and(Vec::is_empty),
        "Codex retained external notifications"
    );
    ensure!(
        config["web_search"].as_str() == Some("live"),
        "The selected Codex policy does not allow live web search for DeLM workers"
    );
    ensure!(
        required["allowedWebSearchModes"].is_null()
            || required["allowedWebSearchModes"]
                .as_array()
                .is_some_and(|modes| modes.iter().any(|mode| mode.as_str() == Some("live"))),
        "Managed policy does not allow live web search for DeLM workers"
    );
    ensure!(
        config["features"]["view_image"].as_bool() == Some(true)
            || config["features"]["view_image"]["enabled"].as_bool() == Some(true),
        "Codex did not enable private image inspection for DeLM workers"
    );
    ensure!(
        config["features"]["default_mode_request_user_input"].as_bool() == Some(true)
            || config["features"]["default_mode_request_user_input"]["enabled"].as_bool()
                == Some(true),
        "Codex does not support worker clarification in Default mode; update Codex before starting DeLM"
    );
    if let Some(servers) = config["mcp_servers"].as_object() {
        ensure!(
            servers
                .values()
                .all(|server| server["enabled"].as_bool() == Some(false)),
            "An external MCP server was added or could not be disabled; start DeLM again after reviewing the configuration"
        );
    }
    ensure!(
        config
            .pointer("/shell_environment_policy/inherit")
            .and_then(Value::as_str)
            == Some("none"),
        "Codex retained inherited tool environment"
    );
    if let Some(values) = config
        .pointer("/shell_environment_policy/set")
        .and_then(Value::as_object)
    {
        ensure!(
            values.values().all(|value| value.as_str() == Some("")),
            "An explicit tool environment variable changed or could not be isolated; start DeLM again"
        );
    }
    let layers = response["layers"]
        .as_array()
        .context("Codex omitted configuration-layer evidence")?;
    ensure!(
        layers
            .iter()
            .all(
                |layer| layer.pointer("/name/type").and_then(Value::as_str) != Some("project")
                    || layer["disabledReason"]
                        .as_str()
                        .is_some_and(|reason| !reason.is_empty())
            ),
        "Codex retained trusted project configuration in a private worker"
    );
    Ok(())
}

struct ProbeDirectory(PathBuf);
impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        // This directory is created exclusively for metadata-only native probes.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Read current native hook trust without opening a thread or generating tokens.
/// A fresh listing does not prove what the parent session already loaded, so it
/// supplements the invocation handshake and monitoring lease, never replaces them.
pub async fn verify_lifecycle_hooks(
    request: &StartRequest,
    binding: &crate::lifecycle::Binding,
) -> Result<()> {
    let path = std::env::temp_dir().join(format!("delm-hook-metadata-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    let probe = ProbeDirectory(path.canonicalize()?);
    let mut config = stock_overrides(&request.auth_settings, &probe.0)?;
    // Hook discovery needs these parent capabilities. No thread or tool executes
    // here, and inherited MCP servers and tool environment remain disabled.
    config["features"]["hooks"] = json!(true);
    config["features"]["plugins"] = json!(true);
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
    let mut request = StartRequest {
        project: project.clone(),
        task,
        context,
        attachments: Vec::new(),
        model: String::new(),
        model_provider: "openai".into(),
        reasoning_effort: None,
        service_tier: None,
        auth_home,
        auth_settings: json!({}),
        host_executable,
        seconds,
        policy: json!({"approval_policy":"never", "sandbox":{"type":"workspace-write","network_access":true},
            "file_system":{"kind":"restricted","entries":[
                {"path":{"type":"special","value":{"kind":"root"}},"access":"read"},
                {"path":{"type":"path","path":project},"access":"write"}]},
            "network":"enabled","network_proxy_active":false}),
    };
    let mut rpc = RpcClient::spawn(&request, &probe.0).await?;
    rpc.initialize().await?;
    let account = rpc
        .request("account/read", json!({"refreshToken":false}))
        .await?;
    request.auth_settings["account_identity"] = account_identity(&account)?;
    request.auth_settings["host_version"] = json!(version);
    let base = rpc
        .request("config/read", json!({"includeLayers":true,"cwd":probe.0}))
        .await?;
    let cfg = &base["config"];
    request.auth_settings["stock_mcp_servers"] = json!(
        cfg["mcp_servers"]
            .as_object()
            .map(|map| map.keys().collect::<Vec<_>>())
            .unwrap_or_default()
    );
    request.auth_settings["stock_environment_keys"] = json!(
        cfg.pointer("/shell_environment_policy/set")
            .and_then(Value::as_object)
            .map(|map| map.keys().collect::<Vec<_>>())
            .unwrap_or_default()
    );
    let project_config = rpc
        .request("config/read", json!({"includeLayers":false,"cwd":project}))
        .await?;
    let cfg = &project_config["config"];
    let parent = if let Ok(id) = std::env::var("CODEX_THREAD_ID") {
        rpc.request("thread/read", json!({"threadId":id,"includeTurns":false}))
            .await
            .ok()
    } else {
        None
    };
    let parent_thread = parent.as_ref().map(|value| &value["thread"]);
    let parent_model = parent_thread
        .and_then(|thread| thread["model"].as_str())
        .map(str::to_owned);
    let source = if model.is_some() {
        "explicit"
    } else if parent_model.is_some() {
        "persisted-parent-thread"
    } else {
        "saved-project-config"
    };
    request.model = if let Some(model) = model
        .or(parent_model)
        .or_else(|| cfg["model"].as_str().map(str::to_owned))
    {
        model
    } else {
        let models = rpc.request("model/list", json!({})).await?;
        request.auth_settings["model_selection_source"] = json!("native-model-default");
        models["data"]
            .as_array()
            .and_then(|models| {
                models
                    .iter()
                    .find(|model| model["isDefault"].as_bool() == Some(true))
            })
            .and_then(|model| model["model"].as_str())
            .context("Codex did not identify a default model; choose --model explicitly")?
            .to_owned()
    };
    if request
        .auth_settings
        .get("model_selection_source")
        .is_none()
    {
        request.auth_settings["model_selection_source"] = json!(source);
    }
    request.reasoning_effort = effort
        .or_else(|| {
            parent_thread
                .and_then(|thread| thread["reasoningEffort"].as_str())
                .map(str::to_owned)
        })
        .or_else(|| cfg["model_reasoning_effort"].as_str().map(str::to_owned));
    request.service_tier = cfg["service_tier"].as_str().map(str::to_owned);
    request.model_provider = parent_thread
        .and_then(|thread| thread["modelProvider"].as_str())
        .or_else(|| cfg["model_provider"].as_str())
        .unwrap_or("openai")
        .to_owned();
    ensure!(
        request.model_provider == "openai",
        "DeLM currently supports the native OpenAI provider only"
    );
    rpc.shutdown(&[]).await?;
    // Reopen with the observed MCP/environment names disabled, then check the
    // actual effective values. Discovery alone is not an isolation guarantee.
    let mut verified = RpcClient::spawn(&request, &probe.0).await?;
    verified.initialize().await?;
    verified.verify_account(&request).await?;
    verified.shutdown(&[]).await?;
    crate::compatibility::qualify(&request).await.with_context(|| {
        format!("Installed {version} did not pass DeLM compatibility checks. No model turn was started. Update Codex or DeLM and retry; your Codex installation was not changed")
    })?;
    Ok(request)
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
    run_dir: PathBuf,
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
        ensure!(
            request.model_provider == "openai",
            "This release supports the native OpenAI provider; no provider was changed."
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
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", std::env::var_os("HOME").context("HOME is unset")?)
            .env("CODEX_HOME", &request.auth_home)
            .env("TERM", "dumb")
            .env("LANG", "en_US.UTF-8")
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
            run_dir,
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
        let requirements = self.request("configRequirements/read", json!({})).await?;
        for cwd in [
            &self.run_dir,
            &self.run_dir.join("workspace/worker-1"),
            &self.run_dir.join("workspace/worker-2"),
        ] {
            if cwd.is_dir() {
                let config = self
                    .request("config/read", json!({"includeLayers":true,"cwd":cwd}))
                    .await?;
                verify_stock_configuration(&config, &requirements)?;
            }
        }
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
        if matches!(method, "thread/start" | "thread/resume") {
            let cwd = params["cwd"]
                .as_str()
                .context("Worker thread needs a private working directory")?
                .to_owned();
            let config = self
                .request_raw("config/read", json!({"includeLayers":true,"cwd":cwd}))
                .await?;
            let requirements = self
                .request_raw("configRequirements/read", json!({}))
                .await?;
            verify_stock_configuration(&config, &requirements)?;
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
        ensure!(threads.len() <= 2, "DeLM owns at most two worker threads");
        let native = tokio::time::timeout(Duration::from_secs(8), async {
            // Dispatch both interrupts before waiting for either worker's tool
            // cleanup, so one slow archive cannot extend its peer's model work.
            let (first, second) = tokio::join!(
                self.interrupt_worker(threads.first()),
                self.interrupt_worker(threads.get(1))
            );
            let mut clean = first && second;
            for (thread, _) in threads {
                if thread.is_empty() {
                    continue;
                }
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
            }
            clean
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
            "Native worker shutdown was not fully acknowledged; preserve both projects"
        );
        Ok(())
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

pub fn worker_config(
    request: &StartRequest,
    run_dir: &Path,
    project: &Path,
    worker: usize,
) -> Result<Value> {
    ensure!((1..=2).contains(&worker), "Worker identity must be 1 or 2");
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
    let kind = native
        .get("kind")
        .and_then(Value::as_str)
        .context("Host supplied an invalid filesystem policy")?;
    ensure!(
        ["restricted", "unrestricted"].contains(&kind),
        "External filesystem sandboxes cannot be safely translated into private worker permissions"
    );
    let unrestricted = kind == "unrestricted";
    let entries = native
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Convert the authorized request's policy representation. We copy no broad
    // write grants: source scopes are mapped into this worker's private tree.
    let mut literals = Vec::<(PathBuf, String)>::new();
    let mut root_access = if unrestricted { "write" } else { "deny" }.to_owned();
    let mut explicit_root = false;
    let mut minimal = unrestricted;
    for entry in &entries {
        let access = entry
            .get("access")
            .and_then(Value::as_str)
            .context("Invalid inherited filesystem access")?;
        ensure!(
            ["read", "write", "deny"].contains(&access),
            "Unknown inherited filesystem access"
        );
        let path = entry
            .get("path")
            .context("Missing inherited filesystem path")?;
        match path.get("type").and_then(Value::as_str) {
            Some("path") => {
                let path = PathBuf::from(
                    path.get("path")
                        .and_then(Value::as_str)
                        .context("Invalid inherited filesystem path")?,
                );
                ensure!(
                    path.is_absolute()
                        && !path
                            .components()
                            .any(|p| matches!(p, std::path::Component::ParentDir)),
                    "Nonlocal inherited permission paths are unsupported"
                );
                let path = canonical_permission_path(&path)?;
                literals.push((path, access.to_owned()));
            }
            Some("glob_pattern") => {
                bail!(
                    "Inherited filesystem globs require exact native and board transfer enforcement; this release refuses that policy without widening it"
                );
            }
            Some("special") => {
                let value = &path["value"];
                match value.get("kind").and_then(Value::as_str) {
                    Some("root") => {
                        let priority = |value: &str| match value {
                            "deny" => 3,
                            "write" => 2,
                            _ => 1,
                        };
                        if !explicit_root || priority(access) > priority(&root_access) {
                            root_access = access.to_owned();
                        }
                        explicit_root = true;
                    }
                    Some("minimal") => {
                        ensure!(
                            access != "deny",
                            "A denied platform runtime cannot support private worker tools"
                        );
                        minimal = true;
                    }
                    Some("project_roots") => {
                        let subpath = value.get("subpath").and_then(Value::as_str).unwrap_or(".");
                        ensure!(
                            !Path::new(subpath).is_absolute()
                                && !Path::new(subpath)
                                    .components()
                                    .any(|p| matches!(p, std::path::Component::ParentDir)),
                            "Invalid inherited project permission subpath"
                        );
                        literals.push((original.join(subpath), access.to_owned()));
                    }
                    Some("tmpdir" | "slash_tmp") => {
                        ensure!(
                            access != "deny",
                            "Explicit temporary-directory denials require a qualified private environment mapping"
                        );
                    }
                    _ => bail!(
                        "Unknown inherited filesystem special path cannot be preserved safely"
                    ),
                }
            }
            _ => bail!("Unknown inherited filesystem policy shape"),
        }
    }
    minimal |= root_access != "deny";
    ensure!(
        minimal,
        "The selected permission policy does not grant the platform reads needed by private worker tools"
    );
    let access_at = |path: &Path| -> Result<&str> {
        let priority = |access: &str| match access {
            "deny" => 3,
            "write" => 2,
            _ => 1,
        };
        let mut matching = Vec::new();
        for (scope, access) in &literals {
            if permission_relative(path, scope)?.is_some() {
                matching.push((scope, access));
            }
        }
        Ok(matching
            .into_iter()
            .max_by_key(|(scope, access)| (scope.components().count(), priority(access)))
            .map(|(_, access)| access.as_str())
            .unwrap_or(&root_access))
    };
    let project_access = access_at(&original)?;
    let mut scoped_write = false;
    for (path, access) in &literals {
        scoped_write |= access == "write"
            && permission_relative(path, &original)?.is_some()
            && access_at(path)? == "write";
    }
    ensure!(
        project_access == "write" || scoped_write,
        "The selected Codex permissions do not allow implementation in this project"
    );
    let env_root = run_dir.join("environment").join(format!("worker-{worker}"));
    let mut filesystem = serde_json::Map::new();
    filesystem.insert(":minimal".into(), json!("read"));
    filesystem.insert(run_dir.to_string_lossy().into_owned(), json!("deny"));
    filesystem.insert(
        project.to_string_lossy().into_owned(),
        json!(project_access),
    );
    filesystem.insert(env_root.to_string_lossy().into_owned(), json!("write"));
    let assets = run_dir.join("attachments");
    if assets.is_dir() {
        filesystem.insert(assets.to_string_lossy().into_owned(), json!("read"));
    }
    let mut readable_toolchains = Vec::new();
    for path in toolchain_roots() {
        let platform = ["/usr", "/bin", "/sbin", "/System"]
            .iter()
            .any(|base| path.starts_with(base));
        let mut explicitly_denied = false;
        for (scope, access) in &literals {
            explicitly_denied |= access == "deny" && permission_relative(&path, scope)?.is_some();
        }
        if permission_relative(&path, &original)?.is_none()
            && permission_relative(&path, &run_dir)?.is_none()
            && (access_at(&path)? != "deny" || (platform && minimal && !explicitly_denied))
        {
            filesystem.insert(path.to_string_lossy().into_owned(), json!("read"));
            readable_toolchains.push(path);
        }
    }
    for (scope, access) in &literals {
        if let Some(relative) = permission_relative(scope, &original)? {
            let mapped: PathBuf = project.join(relative).components().collect();
            let key = mapped.to_string_lossy().into_owned();
            let prior = filesystem.get(&key).and_then(Value::as_str);
            if prior != Some("deny") && !(prior == Some("write") && access == "read") {
                filesystem.insert(key, json!(access));
            }
        }
        if access == "deny" {
            ensure!(
                permission_relative(&project, scope)?.is_none()
                    && permission_relative(&env_root, scope)?.is_none(),
                "Inherited deny rule covers private worker storage"
            );
            filesystem.insert(scope.to_string_lossy().into_owned(), json!("deny"));
        }
    }
    for path in [&request.auth_home, &original] {
        filesystem.insert(path.to_string_lossy().into_owned(), json!("deny"));
    }
    let controls = Path::new("/tmp")
        .canonicalize()?
        .join(format!("delm-{}", unsafe { libc::geteuid() }));
    filesystem.insert(controls.to_string_lossy().into_owned(), json!("deny"));
    let network_policy = request
        .policy
        .get("network")
        .and_then(Value::as_str)
        .context("Host did not supply its effective network policy")?;
    ensure!(
        ["restricted", "enabled"].contains(&network_policy),
        "Unknown inherited network policy"
    );
    let network = network_policy == "enabled";
    if network {
        ensure!(
            request
                .policy
                .get("network_proxy_active")
                .and_then(Value::as_bool)
                == Some(false),
            "Managed or unqualified network restrictions cannot be widened into unrestricted worker network access"
        );
    }
    let profile = format!("delm_worker_{worker}");
    let path = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|path| path.is_absolute())
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| {
            readable_toolchains
                .iter()
                .any(|root| path.starts_with(root))
        })
        .chain(
            ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
                .into_iter()
                .map(PathBuf::from),
        )
        .collect::<Vec<_>>();
    let mut rustup_home = None;
    if let Some(original_home) = std::env::var_os("HOME") {
        let rustup = PathBuf::from(original_home).join(".rustup");
        if rustup.is_dir()
            && readable_toolchains
                .iter()
                .any(|root| rustup.starts_with(root))
        {
            rustup_home = Some(rustup);
        }
    }
    let environment = crate::development::prepare(&env_root, &path, rustup_home.as_deref())?;
    let mut config = stock_overrides(&request.auth_settings, &run_dir)?;
    if !network {
        config["web_search"] = json!("disabled");
    }
    config["default_permissions"] = json!(profile);
    config["permissions"] = json!({profile.clone():{"filesystem":filesystem,"network":{"enabled":network,"allow_local_binding":network}}});
    config["projects"][project.to_string_lossy().as_ref()] = json!({"trust_level":"untrusted"});
    config["shell_environment_policy"]["set"]
        .as_object_mut()
        .context("Invalid private environment")?
        .extend(
            environment
                .as_object()
                .context("Invalid private environment")?
                .clone(),
        );
    config["model_reasoning_effort"] = json!(request.reasoning_effort);
    config["model_provider"] = json!(request.model_provider);
    config["model"] = json!(request.model);
    config["approval_policy"] = json!("never");
    if request.reasoning_effort.is_none() {
        config
            .as_object_mut()
            .unwrap()
            .remove("model_reasoning_effort");
    }
    Ok(config)
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

/// Check the qualified stock response before dispatching a model turn. Its legacy
/// projection exposes write/network bounds; granular denials also require the
/// bound named profile and native sandbox qualification.
pub fn verify_thread_response(
    request: &StartRequest,
    run_dir: &Path,
    project: &Path,
    worker: usize,
    response: &Value,
) -> Result<()> {
    ensure!((1..=2).contains(&worker), "unbound worker identity");
    let project = project.canonicalize()?;
    let environment = run_dir
        .canonicalize()?
        .join("environment")
        .join(format!("worker-{worker}"));
    let cwd = PathBuf::from(
        response["cwd"]
            .as_str()
            .context("Codex omitted effective working directory")?,
    )
    .canonicalize()?;
    ensure!(
        cwd == project,
        "Codex changed the private worker working directory"
    );
    ensure!(
        response["model"].as_str() == Some(request.model.as_str()),
        "Codex changed the selected model"
    );
    ensure!(
        response["modelProvider"].as_str() == Some(request.model_provider.as_str()),
        "Codex changed the selected provider"
    );
    if let Some(effort) = &request.reasoning_effort {
        ensure!(
            response["reasoningEffort"].as_str() == Some(effort.as_str()),
            "Codex changed the selected reasoning effort"
        );
    }
    if let Some(tier) = &request.service_tier {
        ensure!(
            response["serviceTier"].as_str() == Some(tier.as_str()),
            "Codex changed the selected service tier"
        );
    }
    ensure!(
        response["approvalPolicy"].as_str() == Some("never"),
        "Codex did not enforce private worker approval restrictions"
    );
    ensure!(
        response["activePermissionProfile"]["id"].as_str()
            == Some(format!("delm_worker_{worker}").as_str()),
        "Codex did not select the bound private permission profile"
    );
    let sandbox = &response["sandbox"];
    ensure!(
        sandbox["type"].as_str() == Some("workspaceWrite"),
        "Codex did not return a restricted private workspace sandbox"
    );
    ensure!(
        sandbox["excludeTmpdirEnvVar"].as_bool() == Some(true)
            && sandbox["excludeSlashTmp"].as_bool() == Some(true),
        "Codex retained shared temporary-directory write access"
    );
    let network = request.policy["network"].as_str() == Some("enabled");
    ensure!(
        sandbox["networkAccess"].as_bool() == Some(network),
        "Codex changed the effective worker network policy"
    );
    let roots = sandbox["writableRoots"]
        .as_array()
        .context("Codex omitted private writable roots")?;
    let mut environment_present = false;
    for root in roots {
        let root = PathBuf::from(root.as_str().context("Invalid effective writable root")?)
            .canonicalize()?;
        ensure!(
            root.starts_with(&project) || root.starts_with(&environment),
            "Codex granted writable access outside this worker's project and environment"
        );
        environment_present |= root == environment;
    }
    ensure!(
        environment_present,
        "Codex omitted the worker's private environment write scope"
    );
    if let Some(roots) = response["runtimeWorkspaceRoots"].as_array() {
        for root in roots {
            let root = PathBuf::from(root.as_str().context("Invalid runtime workspace root")?)
                .canonicalize()?;
            ensure!(
                root.starts_with(&project) || root.starts_with(&environment),
                "Codex attached an unrelated runtime workspace"
            );
        }
    }
    let environments = response
        .pointer("/thread/environments")
        .and_then(Value::as_array)
        .context("Codex omitted native worker execution-environment evidence")?;
    ensure!(
        environments.len() == 1 && environments[0]["environmentId"].as_str() == Some("local"),
        "Codex selected an execution environment outside the local worker"
    );
    let selected = &environments[0];
    ensure!(
        PathBuf::from(
            selected["cwd"]
                .as_str()
                .context("Codex omitted execution cwd")?
        )
        .canonicalize()?
            == project,
        "Codex changed the local execution working directory"
    );
    let roots = selected["runtimeWorkspaceRoots"]
        .as_array()
        .context("Codex omitted local execution workspace roots")?;
    ensure!(
        !roots.is_empty(),
        "Codex omitted local execution workspace roots"
    );
    for root in roots {
        let root =
            PathBuf::from(root.as_str().context("Invalid local execution root")?).canonicalize()?;
        ensure!(
            root.starts_with(&project) || root.starts_with(&environment),
            "Codex attached an unrelated execution workspace"
        );
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

fn toolchain_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/usr"),
        PathBuf::from("/bin"),
        PathBuf::from("/sbin"),
        PathBuf::from("/System"),
        PathBuf::from("/Library/Developer"),
        PathBuf::from("/Library/Frameworks/Python.framework"),
        PathBuf::from("/opt/homebrew"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for path in [
            ".rustup",
            ".cargo/bin",
            ".nvm/versions/node",
            ".bun/bin",
            ".pyenv/versions",
            ".local/share/fnm/node-versions",
            ".local/share/mise/installs/node",
            ".local/share/mise/installs/python",
        ] {
            roots.push(home.join(path));
        }
    }
    roots
        .into_iter()
        .filter(|p| p.exists())
        .filter_map(|p| p.canonicalize().ok())
        .collect()
}
