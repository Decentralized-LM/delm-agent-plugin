use delm::{
    protocol::StartRequest,
    workers::{stock_overrides, verify_stock_configuration, verify_thread_response, worker_config},
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

struct Fixture {
    _temp: tempfile::TempDir,
    run: PathBuf,
    project: PathBuf,
    request: StartRequest,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let original = base.join("original");
        let run = base.join("run");
        let project = run.join("worker-1");
        let auth = base.join("auth");
        for path in [&original, &project, &auth] {
            fs::create_dir_all(path).unwrap();
        }
        let request = serde_json::from_value(json!({
            "project":original,"task":"Update the selected project","model":"fixture",
            "auth_home":auth,"host_executable":"/usr/bin/false",
            "policy":{"approval_policy":"on-request","sandbox":{"type":"workspace-write","network_access":true},
                "file_system":{"kind":"restricted","entries":[
                    {"path":{"type":"special","value":{"kind":"root"}},"access":"read"},
                    {"path":{"type":"path","path":original},"access":"write"},
                    {"path":{"type":"path","path":original.join("readonly")},"access":"read"},
                    {"path":{"type":"path","path":original.join("private")},"access":"deny"}
                ]},"network":"restricted","network_proxy_active":false}
        })).unwrap();
        Self {
            _temp: temp,
            run,
            project,
            request,
        }
    }
    fn config(&self) -> anyhow::Result<Value> {
        worker_config(&self.request, &self.run, &self.project, 1)
    }
}

#[test]
fn inherited_restrictions_follow_the_private_clone_and_broad_writes_do_not() {
    let fixture = Fixture::new();
    let config = fixture.config().unwrap();
    let filesystem = &config["permissions"]["delm_worker_1"]["filesystem"];
    assert_eq!(filesystem[fixture.project.to_str().unwrap()], "write");
    assert_eq!(
        filesystem[fixture.project.join("readonly").to_str().unwrap()],
        "read"
    );
    assert_eq!(
        filesystem[fixture.project.join("private").to_str().unwrap()],
        "deny"
    );
    assert_eq!(
        filesystem[fixture.request.project.to_str().unwrap()],
        "deny"
    );
    assert_eq!(filesystem[fixture.run.to_str().unwrap()], "deny");
    let controls = PathBuf::from("/tmp")
        .canonicalize()
        .unwrap()
        .join(format!("delm-{}", unsafe { libc::geteuid() }));
    assert_eq!(filesystem[controls.to_str().unwrap()], "deny");
    assert_eq!(
        config["permissions"]["delm_worker_1"]["network"]["enabled"],
        false
    );
    assert_eq!(config["approval_policy"], "never");
    let env = &config["shell_environment_policy"]["set"];
    for key in ["HOME", "TMPDIR", "CARGO_HOME"] {
        assert!(
            PathBuf::from(env[key].as_str().unwrap())
                .starts_with(fixture.run.join("environment/worker-1"))
        );
    }
}

#[test]
fn stock_overrides_disable_named_extensions_and_blank_inherited_environment() {
    let mut fixture = Fixture::new();
    fixture.request.auth_settings = json!({
        "stock_mcp_servers":["external.server"],
        "stock_environment_keys":["NATIVE_SECRET","HOME"]
    });
    let config = fixture.config().unwrap();
    assert_eq!(config["mcp_servers"]["external.server"]["enabled"], false);
    assert_eq!(
        config["shell_environment_policy"]["set"]["NATIVE_SECRET"],
        ""
    );
    assert!(
        PathBuf::from(
            config["shell_environment_policy"]["set"]["HOME"]
                .as_str()
                .unwrap()
        )
        .starts_with(&fixture.run)
    );
    assert_eq!(
        config["projects"][fixture.project.to_str().unwrap()]["trust_level"],
        "untrusted"
    );
    for feature in [
        "hooks",
        "plugins",
        "memories",
        "external_agent_memory_import",
        "multi_agent",
        "multi_agent_v2",
        "recommended_plugins",
    ] {
        assert_eq!(config["features"][feature], false);
    }
}

#[test]
fn stock_config_verification_rejects_capability_drift_before_threads_start() {
    let fixture = Fixture::new();
    let config = stock_overrides(&json!({}), &fixture.run).unwrap();
    let safe = json!({"config":config,"layers":[{"name":{"type":"project"},"disabledReason":"untrusted"}]});
    let requirements = json!({"requirements":null});
    verify_stock_configuration(&safe, &requirements).unwrap();
    for changed in [
        ("/config/features/hooks", json!(true)),
        (
            "/config/features/default_mode_request_user_input",
            json!(false),
        ),
        ("/config/agents/enabled", json!(true)),
        ("/config/notify", json!(["/usr/bin/false"])),
        ("/config/shell_environment_policy/inherit", json!("all")),
    ] {
        let mut unsafe_config = safe.clone();
        *unsafe_config.pointer_mut(changed.0).unwrap() = changed.1;
        assert!(verify_stock_configuration(&unsafe_config, &requirements).is_err());
    }
    let mut enabled_server = safe.clone();
    enabled_server["config"]["mcp_servers"]["new_server"] = json!({"enabled":true});
    assert!(verify_stock_configuration(&enabled_server, &requirements).is_err());
    let mut leaked_environment = safe.clone();
    leaked_environment["config"]["shell_environment_policy"]["set"]["TOKEN"] = json!("secret");
    assert!(verify_stock_configuration(&leaked_environment, &requirements).is_err());
    let mut trusted_project = safe.clone();
    trusted_project["layers"][0]["disabledReason"] = Value::Null;
    assert!(verify_stock_configuration(&trusted_project, &requirements).is_err());
    assert!(
        verify_stock_configuration(
            &safe,
            &json!({"requirements":{"featureRequirements":{"hooks":true}}})
        )
        .is_err()
    );
    assert!(verify_stock_configuration(&safe, &json!({"requirements":{"hooks":{}}})).is_err());
    assert!(
        verify_stock_configuration(
            &safe,
            &json!({"requirements":{"network":{"enabled":false}}})
        )
        .is_err()
    );
    let mut collision = safe.clone();
    collision["config"]["permissions"] = json!({"delm_worker_1":{"filesystem":{"/":"write"}}});
    assert!(verify_stock_configuration(&collision, &requirements).is_err());
    let mut retired_optional = safe.clone();
    retired_optional["config"]["features"]
        .as_object_mut()
        .unwrap()
        .remove("multi_agent_v2");
    verify_stock_configuration(&retired_optional, &requirements).unwrap();
    retired_optional["config"]["features"]
        .as_object_mut()
        .unwrap()
        .remove("multi_agent");
    assert!(verify_stock_configuration(&retired_optional, &requirements).is_err());
    assert!(
        verify_stock_configuration(
            &safe,
            &json!({"requirements":{"allowedWebSearchModes":["cached"]}})
        )
        .is_err()
    );
}

#[tokio::test]
async fn lifecycle_hook_discovery_checks_trust_without_starting_worker_threads() {
    let mut fixture = Fixture::new();
    let host = fixture._temp.path().join("hook-host.py");
    fs::write(&host, include_str!("fixtures/worker_host.py")).unwrap();
    fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).unwrap();
    fixture.request.host_executable = host.clone();
    let executable = fixture._temp.path().join("plugin/bin/delm");
    let hooks_file = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("hooks/hooks.json");
    let identity = json!({"device":0,"inode":0,"size":0,"mode":0,"uid":0,"links":1,
        "modified":[0,0],"changed":[0,0]});
    let binding = serde_json::from_value(json!({
        "session_id":uuid::Uuid::new_v4(),"invocation_id":uuid::Uuid::new_v4(),
        "turn_id":uuid::Uuid::new_v4(),
        "owner":{"pid":42,"started_seconds":0,"started_micros":0,"uid":0},
        "executable":executable,"executable_hash":"fixture","executable_identity":identity,
        "hooks_file":hooks_file,"hooks_hash":"fixture","hooks_identity":identity
    }))
    .unwrap();
    for status in ["trusted", "untrusted"] {
        let hooks = delm::lifecycle::REQUIRED_EVENTS
            .iter()
            .map(|event| {
                json!({
                    "eventName":event,"source":"plugin","sourcePath":hooks_file,
                    "handlerType":"command","enabled":true,"async":false,"trustStatus":status,
                    "matcher":null,"timeoutSec":if *event == "interrupt" {3} else {5},
                    "command":format!("exec \"{}\" lifecycle-hook", executable.display())
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            host.with_extension("json"),
            serde_json::to_vec(&json!({
                "mode":"metadata_only","hook_listing":{"data":[{"hooks":hooks}]}
            }))
            .unwrap(),
        )
        .unwrap();
        let result = delm::workers::verify_lifecycle_hooks(&fixture.request, &binding).await;
        assert_eq!(result.is_ok(), status == "trusted", "{result:?}");
    }
    let wire = fs::read_to_string(host.with_extension("jsonl")).unwrap();
    for record in wire
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
    {
        if record["direction"] == "in" {
            assert!(
                matches!(
                    record["message"]["method"].as_str(),
                    Some("initialize" | "initialized" | "hooks/list")
                ),
                "{record}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires installed stock Codex and native login; no model turn"]
async fn native_stock_account_and_configuration_preflight_without_model_turns() {
    let fixture = Fixture::new();
    let request = delm::workers::stock_request(
        fixture.request.project.clone(),
        "No model turn: configuration probe".into(),
        String::new(),
        None,
        None,
        60,
    )
    .await
    .unwrap();
    assert!(request.auth_settings["account_identity"].is_object());
    assert!(request.auth_settings["stock_mcp_servers"].is_array());
    assert!(request.auth_settings["stock_environment_keys"].is_array());
    assert_eq!(request.policy["network"], "enabled");
    assert!(!request.model.is_empty());
    assert!(
        !request
            .host_executable
            .to_string_lossy()
            .contains("codex-delm")
    );
}

#[test]
fn managed_network_external_sandbox_and_glob_policies_fail_closed() {
    let mut fixture = Fixture::new();
    fixture.request.policy["network"] = json!("enabled");
    fixture.request.policy["network_proxy_active"] = json!(true);
    assert!(
        fixture
            .config()
            .unwrap_err()
            .to_string()
            .contains("Managed")
    );
    fixture.request.policy["network_proxy_active"] = json!(false);
    assert!(fixture.config().is_ok());
    fixture.request.policy["file_system"]["kind"] = json!("external-sandbox");
    assert!(fixture.config().is_err());
    fixture.request.policy["file_system"]["kind"] = json!("restricted");
    fixture.request.policy["file_system"]["entries"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":{"type":"glob_pattern","pattern":"**/.env"},"access":"deny"}));
    assert!(fixture.config().unwrap_err().to_string().contains("globs"));
}

#[test]
fn readonly_host_policy_is_not_promoted_to_private_implementation_authority() {
    let mut fixture = Fixture::new();
    fixture.request.policy["file_system"]["entries"][1]["access"] = json!("read");
    assert!(
        fixture
            .config()
            .unwrap_err()
            .to_string()
            .contains("do not allow implementation")
    );
    fixture.request.policy["file_system"] = json!({"kind":"unrestricted"});
    let config = fixture.config().unwrap();
    let filesystem = config["permissions"]["delm_worker_1"]["filesystem"]
        .as_object()
        .unwrap();
    let writable = filesystem
        .iter()
        .filter(|(_, access)| access.as_str() == Some("write"))
        .map(|(path, _)| PathBuf::from(path))
        .collect::<Vec<_>>();
    assert!(writable.iter().all(|path| path.starts_with(&fixture.run)));
    assert!(writable.contains(&fixture.project));
}

#[cfg(target_os = "macos")]
#[test]
fn source_aliases_and_missing_denied_paths_remain_restricted_after_mapping() {
    let mut fixture = Fixture::new();
    let case_alias = fixture.request.project.parent().unwrap().join("ORIGINAL");
    let alias = if case_alias.is_dir() {
        case_alias
    } else {
        fixture.request.project.clone()
    };
    fs::create_dir(fixture.request.project.join("Protected")).unwrap();
    let entries = fixture.request.policy["file_system"]["entries"]
        .as_array_mut()
        .unwrap();
    entries.push(json!({"path":{"type":"path","path":alias.join("Protected")},"access":"deny"}));
    // The temp fixture is normally under /var; use that alias explicitly to
    // exercise an absent leaf below an existing symlinked ancestor.
    let missing = fixture.request.project.join("future.txt");
    let missing_alias = PathBuf::from(missing.to_string_lossy().replacen(
        "/private/var/",
        "/var/",
        1,
    ));
    entries.push(json!({"path":{"type":"path","path":missing_alias},"access":"deny"}));
    let config = fixture.config().unwrap();
    let filesystem = &config["permissions"]["delm_worker_1"]["filesystem"];
    assert_eq!(
        filesystem[fixture.project.join("Protected").to_str().unwrap()],
        "deny"
    );
    assert_eq!(
        filesystem[fixture.project.join("future.txt").to_str().unwrap()],
        "deny"
    );

    // An aliased deny for the whole project must dominate its literal write.
    fixture.request.policy["file_system"]["entries"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":{"type":"path","path":alias},"access":"deny"}));
    assert!(
        fixture
            .config()
            .unwrap_err()
            .to_string()
            .contains("do not allow implementation")
    );
}

#[test]
fn native_response_must_confirm_private_scope_before_any_model_turn() {
    let fixture = Fixture::new();
    fixture.config().unwrap();
    let environment = fixture.run.join("environment/worker-1");
    let response = json!({"futureField":{"accepted":true},"thread":{"id":"native-thread","environments":[{"environmentId":"local","cwd":fixture.project,"runtimeWorkspaceRoots":[fixture.project]}]},"cwd":fixture.project,"model":fixture.request.model,"modelProvider":"openai","approvalPolicy":"never","activePermissionProfile":{"id":"delm_worker_1"},"reasoningEffort":null,
        "sandbox":{"type":"workspaceWrite","writableRoots":[environment],"networkAccess":false,"excludeTmpdirEnvVar":true,"excludeSlashTmp":true},"runtimeWorkspaceRoots":[fixture.project]});
    verify_thread_response(
        &fixture.request,
        &fixture.run,
        &fixture.project,
        1,
        &response,
    )
    .unwrap();
    let mut unsafe_response = response.clone();
    unsafe_response["sandbox"]["writableRoots"]
        .as_array_mut()
        .unwrap()
        .push(json!(fixture.request.project));
    assert!(
        verify_thread_response(
            &fixture.request,
            &fixture.run,
            &fixture.project,
            1,
            &unsafe_response
        )
        .is_err()
    );
    let mut wrong_approval = response.clone();
    wrong_approval["approvalPolicy"] = json!("on-request");
    assert!(
        verify_thread_response(
            &fixture.request,
            &fixture.run,
            &fixture.project,
            1,
            &wrong_approval
        )
        .is_err()
    );
    let mut wrong_profile = response.clone();
    wrong_profile["activePermissionProfile"]["id"] = json!(":workspace");
    assert!(
        verify_thread_response(
            &fixture.request,
            &fixture.run,
            &fixture.project,
            1,
            &wrong_profile
        )
        .is_err()
    );
    let mut remote = response.clone();
    remote["thread"]["environments"][0]["environmentId"] = json!("remote-worker");
    assert!(
        verify_thread_response(&fixture.request, &fixture.run, &fixture.project, 1, &remote)
            .is_err()
    );
    let mut shared_tmp = response;
    shared_tmp["sandbox"]["excludeSlashTmp"] = json!(false);
    assert!(
        verify_thread_response(
            &fixture.request,
            &fixture.run,
            &fixture.project,
            1,
            &shared_tmp
        )
        .is_err()
    );
}
