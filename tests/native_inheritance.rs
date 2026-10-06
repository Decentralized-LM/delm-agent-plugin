//! Native host qualification. Creates local metadata-only sessions and calls a
//! local documentation MCP fixture; never starts a model turn or opens a browser.
use delm::{
    protocol::StartRequest,
    workers::{
        RpcClient, mcp_manifest, skill_manifest, verify_thread_response, worker_thread_request,
    },
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

const MCP: &str = r#"
import json,sys
for line in sys.stdin:
    message=json.loads(line)
    if 'id' not in message: continue
    method=message.get('method')
    if method=='initialize':
        result={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'fixture-docs','version':'1'}}
    elif method=='tools/list':
        result={'tools':[{'name':'read_docs','description':'Read local fixture docs','inputSchema':{'type':'object','properties':{}}}]}
    elif method=='tools/call':
        result={'content':[{'type':'text','text':'Native inherited documentation works.'}]}
    elif method in ('resources/list','resources/templates/list'):
        result={'resources':[]} if method=='resources/list' else {'resourceTemplates':[]}
    else: result={}
    print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}),flush=True)
"#;

#[tokio::test]
#[ignore = "requires DELM_TEST_HOST pointing to installed Codex; no model or remote tool calls"]
async fn native_fork_preserves_saved_skill_tools_and_approval_policy() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let worker = root.join("run/workspace/worker-1");
    let auth = root.join("codex-home");
    let skill = project.join(".agents/skills/native-example");
    for path in [&skill, &worker, &auth] {
        fs::create_dir_all(path).unwrap();
    }
    let skill_text = "---\nname: native-example\ndescription: Read native fixture documentation.\n---\nUse the fixture docs MCP to read documentation before changing files.\n";
    fs::write(skill.join("SKILL.md"), skill_text).unwrap();
    let worker_skill = worker.join(".agents/skills/native-example");
    fs::create_dir_all(&worker_skill).unwrap();
    fs::write(worker_skill.join("SKILL.md"), skill_text).unwrap();
    let server = root.join("docs-server.py");
    fs::write(&server, MCP).unwrap();
    fs::write(
        auth.join("config.toml"),
        format!(
            "model = \"gpt-5.4\"\napproval_policy = \"on-request\"\nsandbox_mode = \"workspace-write\"\n[mcp_servers.fixture_docs]\ncommand = \"/usr/bin/python3\"\nargs = [{}]\n",
            json!(server.to_str().unwrap())
        ),
    )
    .unwrap();
    let mut request: StartRequest = serde_json::from_value(json!({
        "project":project,"task":"metadata fixture","context":"","model":"gpt-5.4",
        "auth_home":auth,"host_executable":PathBuf::from(std::env::var_os("DELM_TEST_HOST").expect("DELM_TEST_HOST")),
        "policy":{"approval_policy":"on-request","file_system":{"kind":"restricted","entries":[
            {"path":{"type":"special","value":{"kind":"root"}},"access":"read"},
            {"path":{"type":"path","path":project},"access":"write"}
        ]},"network":"restricted","network_proxy_active":false}
    })).unwrap();
    delm::compatibility::qualify(&request).await.unwrap();
    request.auth_settings["native_config_overrides"] =
        json!({"shell_environment_policy":{"set":{"DELM_TRANSIENT_FIXTURE":"parent-only"}}});
    let mut parent = RpcClient::spawn(&request, &root).await.unwrap();
    parent.initialize().await.unwrap();
    let parent_config = parent
        .request("config/read", json!({"cwd":project,"includeLayers":true}))
        .await
        .unwrap();
    assert_eq!(
        parent_config["config"]["shell_environment_policy"]["set"]["DELM_TRANSIENT_FIXTURE"],
        "parent-only"
    );
    let source = parent.request("thread/start", json!({"cwd":project,"model":"gpt-5.4","approvalPolicy":"on-request","sandbox":"workspace-write","ephemeral":false})).await.unwrap();
    let parent_id = source["thread"]["id"].as_str().unwrap().to_owned();
    parent.request("thread/inject_items", json!({"threadId":parent_id,"items":[{"type":"message","role":"user","content":[{"type":"input_text","text":"Metadata-only qualification. No model turn is requested."}]}]})).await.unwrap();
    request.auth_settings["parent_thread_id"] = json!(parent_id);
    request.auth_settings["native_thread_settings"] = json!({"activePermissionProfile":source["activePermissionProfile"],"sandbox":source["sandbox"],"disabledPluginIds":source["disabledPluginIds"]});
    request.reasoning_effort = source["reasoningEffort"].as_str().map(str::to_owned);
    request.service_tier = source["serviceTier"].as_str().map(str::to_owned);
    let source_skills = skill_manifest(
        &parent
            .request("skills/list", json!({"cwds":[project],"forceReload":true}))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        source_skills
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["name"] == "native-example")
    );
    let source_tools = mcp_manifest(&parent, &parent_id).await.unwrap();
    assert!(
        source_tools
            .as_array()
            .unwrap()
            .iter()
            .any(|server| server["name"] == "fixture_docs" && server["tool_count"] == 1)
    );
    // A different app-server reproduces the actual plugin process boundary.
    request
        .auth_settings
        .as_object_mut()
        .unwrap()
        .remove("native_config_overrides");
    let mut child = RpcClient::spawn(&request, &root.join("run")).await.unwrap();
    child.initialize().await.unwrap();
    let child_config = child
        .request("config/read", json!({"cwd":worker,"includeLayers":true}))
        .await
        .unwrap();
    assert!(
        child_config["config"]["shell_environment_policy"]["set"]
            .get("DELM_TRANSIENT_FIXTURE")
            .is_none(),
        "Separate app-server config unexpectedly copied a parent-only CLI override; requalify live-session support"
    );
    let mut gateway =
        delm::worker_tools::Gateway::start(&uuid::Uuid::new_v4().to_string()).unwrap();
    let mut coordination_config = gateway.config(0).unwrap();
    coordination_config["command"] = json!(env!("CARGO_BIN_EXE_delm"));
    let (method, params) = worker_thread_request(
        &request,
        &worker,
        1,
        json!({"mcp_servers":{"delm_coordination_1":coordination_config}}),
        "Coordinate using the board.",
    )
    .unwrap();
    let fork = child.request(method, params).await.unwrap();
    verify_thread_response(&request, &root.join("run"), &worker, 1, &fork).unwrap();
    let child_id = fork["thread"]["id"].as_str().unwrap().to_owned();
    let child_skills = skill_manifest(
        &child
            .request("skills/list", json!({"cwds":[worker],"forceReload":true}))
            .await
            .unwrap(),
    )
    .unwrap();
    let child_tools = mcp_manifest(&child, &child_id).await.unwrap();
    delm::workers::compare_capability_manifests(
        &source_skills,
        &child_skills,
        &source_tools,
        &child_tools,
        Some("delm_coordination_1"),
    )
    .unwrap();
    let docs = child
        .request(
            "mcpServer/tool/call",
            json!({"threadId":child_id,"server":"fixture_docs","tool":"read_docs","arguments":{}}),
        )
        .await
        .unwrap();
    assert!(
        docs.to_string()
            .contains("Native inherited documentation works.")
    );
    let native_call = child.request(
        "mcpServer/tool/call",
        json!({"threadId":child_id,
        "server":"delm_coordination_1","tool":"delm_read","arguments":{}}),
    );
    let serve = async {
        let call = gateway
            .calls
            .recv()
            .await
            .expect("native fork did not reach the DeLM gateway");
        assert_eq!(call.worker, 0);
        assert_eq!(call.thread_id.as_deref(), Some(child_id.as_str()));
        assert!(
            call.call_id.is_none(),
            "Direct MCP calls are transport probes, not model calls"
        );
        assert!(call.turn_id.is_none());
        assert_eq!(call.tool, "delm_read");
        assert_eq!(call.arguments, json!({}));
        call.reply.send(json!({"content":[{"type":"text","text":"Native DeLM board delivery works."}],"isError":false})).unwrap();
    };
    let (board, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(native_call, serve)
    })
    .await
    .expect("native DeLM gateway call timed out");
    assert!(
        board
            .unwrap()
            .to_string()
            .contains("Native DeLM board delivery works.")
    );
    child.shutdown(&[(child_id, None)]).await.unwrap();
    parent.shutdown(&[(parent_id, None)]).await.unwrap();
    if let Some(path) = std::env::var_os("DELM_TEST_EVIDENCE") {
        let report = json!({"kind":"native-inheritance","model_turns":0,"native_protocol_qualified":true,
            "saved_skill_contents_match":true,"saved_mcp_tools_match":true,
            "native_permission_profile_match":true,"mcp_tool_called":true,
            "delm_gateway_tool_called":true,
            "host_version":delm::compatibility::host_version(&request.host_executable).await.unwrap(),
            "architecture":std::env::consts::ARCH,
            "native_test_sha256":format!("{:x}", Sha256::digest(include_bytes!("native_inheritance.rs"))),
            "native_test_binary_sha256":format!("{:x}", Sha256::digest(fs::read(std::env::current_exe().unwrap()).unwrap())),
            "runtime_sha256":format!("{:x}", Sha256::digest(fs::read(env!("CARGO_BIN_EXE_delm")).unwrap())),
            "source_digest":format!("{:x}", Sha256::digest(concat!(include_str!("native_inheritance.rs"), include_str!("../src/workers.rs"), include_str!("../src/worker_tools.rs"), include_str!("../src/compatibility.rs")).as_bytes())),
            "exact_live_session_parity":false,"parent_cli_overrides_not_exported":true,
            "skills":child_skills,"mcp_servers":child_tools});
        fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
