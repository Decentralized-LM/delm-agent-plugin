#!/usr/bin/python3
"""Deterministic app-server lifecycle fixture. No model or network access.

Each test copies this script beside its own JSON configuration and durable state.
The wire log records requests and responses, including those whose ACK is lost.
"""
import json
import os
import pathlib
import sys

fixture = pathlib.Path(__file__)
config = json.loads(fixture.with_suffix(".json").read_text())
mode = config["mode"]

if "--version" in sys.argv:
    print(config.get("version", "codex-cli 99.0.0"))
    sys.exit(0)

if "generate-json-schema" in sys.argv:
    methods = {
        "thread/start": ["cwd", "model", "modelProvider", "serviceTier", "approvalPolicy", "sandbox", "permissions", "config", "developerInstructions", "dynamicTools", "ephemeral", "environments", "runtimeWorkspaceRoots"],
        "thread/resume": ["threadId", "cwd", "model", "modelProvider", "serviceTier", "permissions", "config", "runtimeWorkspaceRoots"],
        "thread/fork": ["threadId", "cwd", "config", "developerInstructions", "beforeTurnId", "deferGoalContinuation", "excludeTurns", "ephemeral", "runtimeWorkspaceRoots"],
        "thread/read": ["threadId", "includeTurns"],
        "thread/unsubscribe": ["threadId"],
        "config/read": ["cwd", "includeLayers"],
        "configRequirements/read": [],
        "skills/list": ["cwds", "forceReload"],
        "hooks/list": ["cwds"],
        "skills/extraRoots/set": ["extraRoots"],
        "mcpServerStatus/list": ["threadId", "detail", "limit", "cursor"],
        "experimentalFeature/list": ["cursor", "limit"],
        "turn/start": ["threadId", "input"],
        "turn/steer": ["threadId", "expectedTurnId", "input"],
        "turn/interrupt": ["threadId", "turnId"],
        "thread/backgroundTerminals/clean": ["threadId"],
        "thread/archive": ["threadId"],
        "command/exec": ["command", "cwd", "permissionProfile", "timeoutMs", "outputBytesCap"],
    }
    if mode == "missing_compatibility_method":
        del methods["turn/interrupt"]

    def schema(methods):
        return {"oneOf": [{"properties": {
            "method": {"enum": [method]},
            "params": {"properties": {field: {} for field in fields}},
        }} for method, fields in methods.items()]}

    output = pathlib.Path(sys.argv[sys.argv.index("--out") + 1])
    output.mkdir(parents=True)
    (output / "ClientRequest.json").write_text(json.dumps(schema(methods)))
    (output / "ServerRequest.json").write_text(json.dumps(schema({
        "item/tool/call": ["threadId", "turnId", "callId", "tool", "arguments"],
        "item/tool/requestUserInput": ["threadId", "turnId", "itemId", "questions"],
        "item/commandExecution/requestApproval": ["threadId", "turnId", "itemId", "availableDecisions"],
        "item/fileChange/requestApproval": ["threadId", "turnId", "itemId"],
        "item/permissions/requestApproval": ["threadId", "turnId", "itemId", "permissions"],
        "mcpServer/elicitation/request": ["threadId", "turnId", "serverName"],
    })))
    notifications = schema({
        "item/started": ["threadId", "turnId", "item"],
        "item/completed": ["threadId", "turnId", "item"],
        "turn/completed": ["threadId", "turn"],
        "serverRequest/resolved": ["threadId", "requestId"],
    })
    notifications["oneOf"][0]["properties"]["params"]["properties"]["item"] = {"oneOf": [{"properties": {
        "type": {"enum": ["mcpToolCall"]}, "id": {"type": "string"},
        "server": {"type": "string"}, "tool": {"type": "string"}, "arguments": {},
    }}]}
    (output / "ServerNotification.json").write_text(json.dumps(notifications))
    sys.exit(0)


def override_value(value):
    """Parse only the JSON-shaped inline TOML emitted by the runtime fixture client."""
    output = []
    quoted = False
    escaped = False
    for character in value:
        if quoted:
            output.append(character)
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quoted = False
        else:
            quoted = character == '"'
            output.append(":" if character == "=" else character)
    return json.loads("".join(output))


effective_config = {"model": "fixture", "model_provider": "openai"}
for index, argument in enumerate(sys.argv[:-1]):
    if argument in ("--config", "-c"):
        key, value = sys.argv[index + 1].split("=", 1)
        effective_config[key] = override_value(value)

state_path = fixture.with_suffix(".state.json")
wire_path = fixture.with_suffix(".jsonl")
state = json.loads(state_path.read_text()) if state_path.exists() else {
    "threads": {}, "turns": 0
}
threads = state["threads"]
calls = {}
active = {}
serial = 0
stale_sent = set()


def log(direction, message):
    with wire_path.open("a") as output:
        output.write(json.dumps({"pid": os.getpid(), "direction": direction,
                                 "message": message}) + "\n")


def persist():
    state_path.write_text(json.dumps(state))


def send(value):
    log("out", value)
    print(json.dumps(value), flush=True)


def native_thread(thread, params):
    inherited = config.get("native_settings", {})
    cwd = params["cwd"]
    return {
        "thread": {"id": thread, "environments": [{"environmentId":"local", "cwd":cwd, "runtimeWorkspaceRoots":[cwd]}]},
        "model": params.get("model", effective_config.get("model", "fixture")),
        "modelProvider": params.get("modelProvider", "openai"), "cwd": cwd,
        "approvalPolicy": params.get("approvalPolicy", inherited.get("approvalPolicy", "never")),
        "activePermissionProfile": inherited.get("activePermissionProfile", {"id": ":workspace"}),
        "reasoningEffort": params.get("config", {}).get("model_reasoning_effort"),
        "serviceTier": params.get("serviceTier"),
        "disabledPluginIds": [], "instructionSources": [],
        "runtimeWorkspaceRoots": params.get("runtimeWorkspaceRoots", [cwd]),
        "sandbox": inherited.get("sandbox", {"type": "workspaceWrite", "writableRoots": [cwd],
                    "excludeTmpdirEnvVar": False, "excludeSlashTmp": False, "networkAccess": True}),
    }


def tool(thread, turn, name, arguments, stage):
    global serial
    serial += 1
    call = "fixture-{}-{}".format(os.getpid(), serial)
    if name == "delm_complete" or arguments:
        arguments["idempotency_key"] = call
    calls[call] = (thread, turn, stage)
    send({"id": call, "method": "item/tool/call", "params": {
        "threadId": thread, "turnId": turn, "callId": call,
        "tool": name, "arguments": arguments}})


def status(thread, turn):
    tool(thread, turn, "delm_status", {}, "status")


def approval(thread, turn):
    global serial
    serial += 1
    request = "approval-" + str(serial)
    calls[request] = (thread, turn, "approval")
    send({"id": request, "method": "item/commandExecution/requestApproval", "params": {
        "threadId": thread, "turnId": turn, "itemId": request,
        "command": "fixture-check", "cwd": threads[thread]["cwd"],
        "availableDecisions": ["accept", "decline"], "startedAtMs": 1}})


def complete(thread, turn, revision):
    path = pathlib.Path(threads[thread]["cwd"])
    (path / "result.txt").write_text(thread + "\n")
    if revision > 1:
        (path / "revised.txt").write_text("revision {}\n".format(revision))
    expected = revision
    if mode == "stale_revision" and thread not in stale_sent:
        stale_sent.add(thread)
        expected = revision - 1
    tool(thread, turn, "delm_complete", {
        "expected_revision": expected, "outcome": "complete",
        "summary": "Created result.txt for revision {}. Lifecycle fixture; no command checks were needed.".format(revision),
        "checks": [],
    }, "completion")


for line in sys.stdin:
    message = json.loads(line)
    log("in", message)
    method = message.get("method")
    params = message.get("params", {})
    request_id = message.get("id")
    if method == "initialize":
        if mode != "block_initialize":
            send({"id": request_id, "result": {"userAgent": "fixture"}})
    elif method == "account/read":
        workspace = "different-workspace" if mode == "mismatch_account" else "fixture-workspace"
        send({"id": request_id, "result": {
            "account": {"type": "chatgpt", "email": "fixture@localhost", "planType": "plus"},
            "workspaceRouting": {"chatgptAccountId": workspace},
            "requiresOpenaiAuth": True,
        }})
    elif method == "config/read":
        send({"id": request_id, "result": {"config": effective_config, "layers": []}})
    elif method == "configRequirements/read":
        send({"id": request_id, "result": {"requirements": None}})
    elif method == "hooks/list":
        send({"id": request_id, "result": config.get("hook_listing", {"data": []})})
    elif method == "experimentalFeature/list":
        send({"id":request_id, "result":{"data":[{"name":"default_mode_request_user_input", "stage":"underDevelopment", "enabled":True}],"nextCursor":None}})
    elif method == "model/list":
        send({"id": request_id, "result": {"data": [{"id": "fixture", "model": "fixture", "isDefault": True}], "nextCursor": None}})
    elif method == "thread/read":
        send({"id": request_id, "result": config.get("parent_history", {"thread":{"id":params["threadId"],"turns":[]}})})
    elif method == "skills/list":
        send({"id": request_id, "result": config.get("skill_listing", {"data":[{"cwd":params.get("cwds", [""])[0], "skills":[], "errors":[]}]})})
    elif method == "skills/extraRoots/set":
        send({"id": request_id, "result": {}})
    elif method == "mcpServerStatus/list":
        send({"id": request_id, "result": config.get("mcp_listing", {"data":[],"nextCursor":None})})
    elif method == "command/exec":
        if mode == "failed_compatibility_probe":
            send({"id": request_id, "result": {"exitCode": 1, "stdout": "", "stderr": "fixture sandbox is incompatible"}})
            continue
        environment = effective_config["shell_environment_policy"]["set"]
        for key in ("HOME", "TMPDIR"):
            (pathlib.Path(environment[key]) / "canary").write_text("probe write\n")
        send({"id": request_id, "result": {"exitCode": 0, "stdout": "delm-isolation-ok\n", "stderr": "", "futureField": True}})
    elif method in ("thread/start", "thread/resume", "thread/fork"):
        if params.get("environments") == []:
            send({"id": request_id, "error": {"code": -32602,
                                              "message": "Empty environments disables native filesystem and exec tools"}})
            continue
        if params.get("ephemeral"):
            thread = "ephemeral-compatibility"
        elif method in ("thread/start", "thread/fork"):
            thread = "thread-" + str(len(threads) + 1)
        else:
            thread = params["threadId"]
            if thread not in threads:
                send({"id": request_id, "error": {"code": -32000,
                                                  "message": "Unknown saved thread"}})
                continue
        if not params.get("ephemeral"):
            threads[thread] = params
            persist()
        response = native_thread(thread, params)
        if mode == "mismatch_model":
            response["model"] = "unexpected-model"
        elif mode == "mismatch_profile":
            response["activePermissionProfile"]["id"] = "wrong-profile"
        send({"id": request_id, "result": response})
    elif method == "turn/start":
        state["turns"] += 1
        turn = "turn-" + str(state["turns"])
        thread = params["threadId"]
        active[thread] = turn
        persist()
        if mode == "lose_turn_ack":
            continue
        send({"id": request_id, "result": {"turn": {"id": turn}}})
        if mode == "approvals":
            approval(thread, turn)
        elif mode == "stale_approval":
            approval(thread, "retired-turn")
            status(thread, turn)
        elif mode == "late_approval" and thread == "thread-2":
            pass
        elif mode == "questions":
            serial += 1
            question = "question-" + str(serial)
            calls[question] = (thread, turn, "question")
            send({"id": question, "method": "item/tool/requestUserInput", "params": {
                "threadId": thread, "turnId": turn, "itemId": question,
                "questions": [{"id": "format", "header": "Output", "question": "Which format?",
                    "options": [{"label": "SVG", "description": "Editable vector"},
                                {"label": "PNG", "description": "Raster image"}]}]}})
        elif mode != "wait":
            status(thread, turn)
    elif method == "turn/steer":
        thread, turn = params["threadId"], params["expectedTurnId"]
        active[thread] = turn
        send({"id": request_id, "result": {"turnId": turn}})
        if mode in ("wait", "questions"):
            tool(thread, turn, "delm_status", {
                "summary": "Fixture received the user update.", "state": "working"
            }, "notice")
        else:
            status(thread, turn)
    elif method in ("turn/interrupt", "thread/backgroundTerminals/clean", "thread/archive", "thread/unsubscribe"):
        send({"id": request_id, "result": {}})
    elif method is None and request_id in calls:
        thread, turn, stage = calls.pop(request_id)
        if active.get(thread) != turn or stage == "notice":
            continue
        result = message.get("result", {})
        if stage == "approval":
            send({"method": "serverRequest/resolved", "params": {"threadId": thread, "requestId": request_id}})
            if result.get("decision") == "accept":
                status(thread, turn)
            continue
        if stage == "question":
            send({"method": "serverRequest/resolved", "params": {"threadId": thread, "requestId": request_id}})
            continue
        content = result.get("contentItems", [])
        body = json.loads(content[0]["text"]) if content else {}
        if stage == "status":
            if result.get("success"):
                complete(thread, turn, body["board"]["request_revision"])
        elif result.get("success"):
            active.pop(thread, None)
            send({"method": "turn/completed", "params": {
                "threadId": thread, "turn": {"id": turn, "status": "completed"}}})
            if mode == "late_approval" and thread == "thread-1":
                approval("thread-2", active["thread-2"])
        elif "board" in body:
            # A rejected stale declaration cannot complete the native turn.
            complete(thread, turn, body["board"]["request_revision"])
    elif method is not None and request_id is not None:
        send({"id": request_id, "error": {"code": -32601, "message": method}})
