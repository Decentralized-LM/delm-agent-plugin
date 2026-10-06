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
fn board_permissions_map_project_rules_without_stripping_native_capabilities() {
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
    for key in [
        "features",
        "agents",
        "skills",
        "cloud",
        "mcp_servers",
        "shell_environment_policy",
        "approval_policy",
    ] {
        assert!(
            config.get(key).is_none(),
            "board scope must not replace native {key}"
        );
    }
}

#[test]
fn ordinary_configuration_and_managed_integrations_are_preserved() {
    let fixture = Fixture::new();
    let native = json!({"features":{"plugins":true,"hooks":true,"multi_agent":true,"memories":true},
        "mcp_servers":{"docs":{"command":"docs-server","enabled":true}},
        "skills":{"include_instructions":true},"shell_environment_policy":{"inherit":"all","set":{"PROJECT_FEATURE":"enabled"}},
        "notify":["native-notifier"],"approval_policy":"on-request"});
    assert_eq!(
        stock_overrides(&json!({"native_config_overrides":native}), &fixture.run).unwrap(),
        native
    );
    assert_eq!(
        stock_overrides(&json!({}), &fixture.run).unwrap(),
        json!({})
    );
    verify_stock_configuration(
        &json!({"config":native}),
        &json!({"requirements":{"hooks":{},"network":{},"featureRequirements":{"plugins":true}}}),
    )
    .unwrap();
    assert!(verify_stock_configuration(&json!({}), &json!({"requirements":null})).is_err());
}

#[test]
fn metadata_probes_do_not_combine_ephemeral_with_goal_deferral() {
    let fixture = Fixture::new();
    for parent in [None, Some("parent")] {
        let (method, params) =
            delm::workers::metadata_thread_request(&fixture.project, json!({}), parent);
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
        if let Some(parent) = parent {
            assert_eq!(params["threadId"], parent);
            assert_eq!(params["excludeTurns"], true);
        }
    }
}

#[test]
fn native_fork_adds_coordination_without_replacing_permissions_or_plugins() {
    let mut fixture = Fixture::new();
    fixture.request.auth_settings = json!({"parent_thread_id":"parent","parent_turn_id":"invocation", "saved_developer_instructions":"Keep existing conventions.",
        "native_config_overrides":{"mcp_servers":{"docs":{"command":"docs-server"}},"features":{"hooks":true,"plugins":true}}});
    let mut additions = fixture.config().unwrap();
    additions["mcp_servers"] =
        json!({"delm_coordination_1":{"command":"delm","args":["worker-mcp"]}});
    let (method, params) = delm::workers::worker_thread_request(
        &fixture.request,
        &fixture.project,
        1,
        additions,
        "Collaborate on the task.",
    )
    .unwrap();
    assert_eq!(method, "thread/fork");
    assert_eq!(params["beforeTurnId"], "invocation");
    assert_eq!(params["deferGoalContinuation"], true);
    assert_eq!(
        params["config"]["mcp_servers"]["docs"]["command"],
        "docs-server"
    );
    assert_eq!(
        params["config"]["mcp_servers"]["delm_coordination_1"]["command"],
        "delm"
    );
    assert_eq!(
        params["developerInstructions"],
        "Keep existing conventions.\n\nCollaborate on the task."
    );
    for field in [
        "permissions",
        "approvalPolicy",
        "sandbox",
        "baseInstructions",
        "dynamicTools",
    ] {
        assert!(params.get(field).is_none());
    }
    assert!(params["config"].get("permissions").is_none());
    assert!(params["config"].get("default_permissions").is_none());
}

#[test]
fn coordination_server_never_overwrites_a_saved_or_plugin_integration() {
    let mut fixture = Fixture::new();
    let additions = json!({"mcp_servers":{"delm_coordination_1":{"command":"delm"}}});
    for evidence in [
        json!({"native_config_overrides":{"mcp_servers":{"delm_coordination_1":{"command":"ordinary","enabled":false}}}}),
        json!({"configured_mcp_server_names":["delm_coordination_1"]}),
        json!({"mcp_manifest":[{"name":"delm_coordination_1","plugin_id":"ordinary-plugin"}]}),
    ] {
        fixture.request.auth_settings = evidence;
        let error = delm::workers::worker_thread_request(
            &fixture.request,
            &fixture.project,
            1,
            additions.clone(),
            "Coordinate",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot replace an existing integration")
        );
    }
    assert!(
        delm::workers::compare_capability_manifests(
            &json!([]),
            &json!([]),
            &json!([]),
            &json!([{"name":"delm_coordination_unrelated","tools_sha256":"ordinary"}])
        )
        .is_err()
    );
}

#[test]
fn invocation_inputs_keep_images_mentions_and_text_in_native_order() {
    let content = json!([{"type":"text","text":"Build from this sketch"}, {"type":"image","fileId":"native-file","detail":"original"},
        {"type":"skill","name":"design","path":"/skills/design/SKILL.md"}, {"type":"localImage","path":"/tmp/sketch.png"}]);
    let history = json!({"thread":{"turns":[{"id":"chosen","itemsView":"full","items":[{"type":"userMessage","content":content}]}]}});
    assert_eq!(
        delm::workers::invocation_inputs(&history, "chosen")
            .unwrap()
            .unwrap(),
        content.as_array().unwrap().clone()
    );
    assert!(
        delm::workers::invocation_inputs(&history, "different")
            .unwrap()
            .is_none()
    );
    let mut summarized = history;
    summarized["thread"]["turns"][0]["itemsView"] = json!("summary");
    assert!(delm::workers::invocation_inputs(&summarized, "chosen").is_err());
}

#[test]
fn skill_content_and_mcp_tool_definitions_must_match_not_only_names() {
    let skill = json!({"name":"frontend-design","enabled":true,"plugin_id":null,"instructions_sha256":"same","dependencies_sha256":"same"});
    let server = json!({"name":"docs","plugin_id":null,"auth_status":"notLoggedIn","tools_sha256":"same","discovery_failed":false});
    let expected_skills = json!([skill]);
    let expected_tools = json!([server]);
    delm::workers::compare_capability_manifests(
        &expected_skills,
        &expected_skills,
        &expected_tools,
        &expected_tools,
    )
    .unwrap();
    let mut changed = expected_skills.clone();
    changed[0]["instructions_sha256"] = json!("changed");
    assert!(
        delm::workers::compare_capability_manifests(
            &expected_skills,
            &changed,
            &expected_tools,
            &expected_tools
        )
        .is_err()
    );
    let mut changed = expected_tools.clone();
    changed[0]["tools_sha256"] = json!("changed");
    assert!(
        delm::workers::compare_capability_manifests(
            &expected_skills,
            &expected_skills,
            &expected_tools,
            &changed
        )
        .is_err()
    );
    assert!(
        delm::workers::compare_capability_manifests(
            &expected_skills,
            &json!([]),
            &expected_tools,
            &expected_tools
        )
        .is_err()
    );
}

#[test]
fn capability_parity_detects_enablement_dependencies_auth_and_discovery_changes() {
    let skills = json!([
        {"name":"design","enabled":true,"plugin_id":"design-plugin","instructions_sha256":"instructions","dependencies_sha256":"dependencies"},
        {"name":"disabled","enabled":false,"plugin_id":null,"instructions_sha256":null,"dependencies_sha256":"none"}
    ]);
    let tools = json!([{"name":"docs","plugin_id":"docs-plugin","auth_status":"oAuth","tools_sha256":"tools","discovery_failed":false}]);
    for (field, value) in [
        ("enabled", json!(false)),
        ("dependencies_sha256", json!("changed")),
        ("plugin_id", json!("another-plugin")),
    ] {
        let mut actual = skills.clone();
        actual[0][field] = value;
        assert!(
            delm::workers::compare_capability_manifests(&skills, &actual, &tools, &tools).is_err(),
            "accepted changed skill {field}"
        );
    }
    let mut enabled = skills.clone();
    enabled[1]["enabled"] = json!(true);
    assert!(
        delm::workers::compare_capability_manifests(&skills, &enabled, &tools, &tools).is_err()
    );
    for (field, value) in [
        ("auth_status", json!("notLoggedIn")),
        ("discovery_failed", json!(true)),
        ("plugin_id", json!("another-plugin")),
    ] {
        let mut actual = tools.clone();
        actual[0][field] = value;
        assert!(
            delm::workers::compare_capability_manifests(&skills, &skills, &tools, &actual).is_err(),
            "accepted changed MCP {field}"
        );
    }
    let mut reordered = skills.clone();
    reordered.as_array_mut().unwrap().reverse();
    delm::workers::compare_capability_manifests(&skills, &reordered, &tools, &tools).unwrap();
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
        fixture.request.auth_settings["startup_hook_listing"] = json!({"data":[{"hooks":hooks}]});
        fixture.request.host_executable = PathBuf::from("/missing-host-must-not-be-launched");
        let cached = delm::workers::verify_lifecycle_hooks(&fixture.request, &binding).await;
        assert_eq!(cached.is_ok(), status == "trusted", "{cached:?}");
        fixture
            .request
            .auth_settings
            .as_object_mut()
            .unwrap()
            .remove("startup_hook_listing");
        fixture.request.host_executable = host.clone();
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
    assert!(request.auth_settings["skills_manifest"].is_array());
    assert!(request.auth_settings["mcp_manifest"].is_array());
    assert_eq!(
        request.auth_settings["capability_report"]["exact_live_session_parity"],
        false
    );
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
        fixture.config().is_ok(),
        "managed policy is inherited natively rather than rewritten"
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
fn native_response_confirms_inherited_settings_without_forcing_never() {
    let mut fixture = Fixture::new();
    let sandbox = json!({"type":"workspaceWrite","writableRoots":[],"networkAccess":false});
    fixture.request.auth_settings["native_thread_settings"] = json!({"activePermissionProfile":{"id":"native-project"},"sandbox":sandbox,"disabledPluginIds":[]});
    let response = json!({"thread":{"id":"native-thread"},"cwd":fixture.project,"model":fixture.request.model,"modelProvider":"openai", "approvalPolicy":"on-request", "activePermissionProfile":{"id":"native-project"},"sandbox":sandbox,"disabledPluginIds":[]});
    verify_thread_response(
        &fixture.request,
        &fixture.run,
        &fixture.project,
        1,
        &response,
    )
    .unwrap();
    for (key, value) in [
        ("approvalPolicy", json!("never")),
        ("model", json!("wrong-model")),
        ("activePermissionProfile", json!({"id":"delm_worker_1"})),
        ("disabledPluginIds", json!(["user-plugin"])),
    ] {
        let mut changed = response.clone();
        changed[key] = value;
        assert!(
            verify_thread_response(
                &fixture.request,
                &fixture.run,
                &fixture.project,
                1,
                &changed
            )
            .is_err(),
            "{key}"
        );
    }
}

#[test]
fn nested_worker_launch_is_rejected_before_auth_or_model_work() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_delm"))
        .arg("--stdio")
        .env("DELM_WORKER_SESSION", "1")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("already a DeLM worker"));
    assert!(result.stdout.is_empty());
}
