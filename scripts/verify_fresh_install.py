#!/usr/bin/env python3
"""Explicit real-model qualification in a fresh native plugin/config installation.

Reuses an existing file-backed native login through an auth-store symlink. Never
reads, copies, or archives credentials. This does not qualify new-account login.
Requires explicit authorization for real model use and the exact plugin hooks.
"""
import argparse
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import time

from build import stage_package
from verify_native_lifecycle import RPC


SOURCE = Path(__file__).resolve().parent.parent
SENSITIVE = {"account", "account_identity", "workspaceRouting", "email", "tokens",
             "access_token", "refresh_token", "id_token", "OPENAI_API_KEY"}


def redacted(value):
    if isinstance(value, dict):
        return {key: ("[redacted]" if key in SENSITIVE else redacted(item))
                for key, item in value.items()}
    if isinstance(value, list):
        return [redacted(item) for item in value]
    return value


def save(path, value):
    path.write_text(json.dumps(redacted(value), indent=2) + "\n")


def snapshot(root):
    """Only use on the deliberately created source fixture, never a Codex home."""
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            result[str(path.relative_to(root))] = {"link": str(path.readlink())}
        elif path.is_file():
            result[str(path.relative_to(root))] = {
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "mode": path.stat().st_mode & 0o7777}
        elif path.is_dir():
            result[str(path.relative_to(root))] = {"directory": True,
                                                  "mode": path.stat().st_mode & 0o7777}
    return result


def read_task(path):
    # Text IO's universal-newline conversion would hide CRLF changes.
    return path.read_bytes().decode("utf-8")


def task_handoff(expected, actual):
    """A terminal LF is message framing; preserve every other byte."""
    exact = expected == actual
    same_content = expected.removesuffix("\n") == actual.removesuffix("\n")
    return {"task_exact_bytes": exact, "task_content_unchanged": same_content,
            "task_terminal_newline_normalized": same_content and not exact}


def git_state(files):
    """Project delivery must leave the existing branch, objects, and index alone."""
    return {path: value for path, value in files.items()
            if path == ".git" or path.startswith(".git/")}


def identity_running(expected):
    """Compare Darwin process birth identity, never just a reusable PID."""
    class BsdInfo(ctypes.Structure):
        _fields_ = [(name, ctypes.c_uint32) for name in [
            "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid",
            "rgid", "svuid", "svgid", "reserved"]] + [
                ("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)] + [
                    (name, ctypes.c_uint32) for name in ["nfiles", "pgid", "pjobc", "tdev", "tpgid"]] + [
                        ("nice", ctypes.c_int32), ("started_seconds", ctypes.c_uint64),
                        ("started_micros", ctypes.c_uint64)]
    # Layout and flavor: macOS SDK sys/proc_info.h, PROC_PIDTBSDINFO.
    library = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
    library.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                    ctypes.c_void_p, ctypes.c_int]
    library.proc_pidinfo.restype = ctypes.c_int
    info = BsdInfo()
    ctypes.set_errno(0)
    count = library.proc_pidinfo(expected["pid"], 3, 0, ctypes.byref(info), ctypes.sizeof(info))
    if count == 0 and ctypes.get_errno() in [errno.ESRCH, errno.ENOENT]:
        return False
    if count != ctypes.sizeof(info):
        raise RuntimeError("Native process identity could not be inspected")
    actual = {name: int(getattr(info, name))
              for name in ["pid", "started_seconds", "started_micros", "uid"]}
    return actual == expected and info.status != 5


def remove_auth_reference(home, runs_root, clients, timeout=15):
    """Never unlink a login reference while an owned native host may need it."""
    until = time.monotonic() + timeout
    while True:
        watchdogs = [json.loads(path.read_text())
                     for path in runs_root.glob("*/launches/*/watchdog.json")]
        reports = [json.loads(path.read_text())
                   for path in runs_root.glob("*/launches/*/shutdown-report.json")]
        identities = [watchdog[key] for watchdog in watchdogs for key in ["host", "runtime"]]
        identities.extend(identity for report in reports for identity in report["owned_processes"])
        stopped = all(client.process.poll() is not None for client in clients)
        stopped &= all(not identity_running(identity) for identity in identities)
        if stopped:
            reference = home / "auth.json"
            if reference.is_symlink():
                reference.unlink()
                return True
            return not reference.exists()
        if time.monotonic() >= until:
            return False
        time.sleep(0.1)


class EvidenceRPC(RPC):
    def __init__(self, *args):
        self.outgoing = []
        super().__init__(*args)

    def send(self, value):
        if value.get("method") == "initialize":
            # Current app-server clients receive standard MCP form requests.
            # Declare its documented form extension as well; never use the
            # model's request-user-input tool to choose a team size.
            value = json.loads(json.dumps(value))
            value["params"]["capabilities"].update({
                "experimentalApi": True, "extensions": {"openai/form": {}}})
        self.outgoing.append(redacted(value))
        super().send(value)


def qualification_hooks(listing, trust):
    hooks = [hook for entry in listing["data"] for hook in entry["hooks"]]
    expected = {("preToolUse", "command"), ("userPromptSubmit", "command"),
                ("userPromptSubmit", "mcpTool"), ("interrupt", "command"), ("stop", "command")}
    if (len(hooks) != 5 or {(hook.get("eventName"), hook.get("handlerType")) for hook in hooks} != expected
            or any(hook.get("trustStatus") != trust or not hook.get("enabled") for hook in hooks)):
        raise RuntimeError("The complete native DeLM hook and selector contract was not registered")
    selector = next(hook for hook in hooks if hook["handlerType"] == "mcpTool")
    if selector.get("server") != "delm_selector" or selector.get("tool") != "select_agents":
        raise RuntimeError("The native DeLM selector hook does not identify the installed selector")
    for hook in hooks:
        timeout = 330 if hook["eventName"] == "userPromptSubmit" else 3 if hook["eventName"] == "interrupt" else 5
        if hook.get("timeoutSec") != timeout:
            raise RuntimeError("The native DeLM hook timeout does not match the selection contract")
    return hooks


def selector_response(message, agents, thread, turn):
    """Replay only this qualification's explicitly chosen native form answer.

    Other permissions, forms, threads, and changed schemas are never approved.
    This function is a test client, not part of the installed plugin.
    """
    if type(agents) is not int or agents not in (2, 3, 4):
        raise ValueError("Choose 2, 3, or 4 qualification agents")
    if message.get("method") != "mcpServer/elicitation/request":
        raise ValueError("Expected the native MCP selector request")
    request_id, params = message.get("id"), message.get("params", {})
    if (type(request_id) not in (str, int) or not isinstance(params, dict)
            or params.get("serverName") != "delm_selector" or params.get("threadId") != thread
            or params.get("turnId") not in (None, turn) or params.get("mode") != "form"
            or params.get("message") != "How many agents?"):
        raise ValueError("Refusing a form outside this DeLM invocation")
    schema = params.get("requestedSchema")
    properties = schema.get("properties") if isinstance(schema, dict) else None
    field = properties.get("agents") if isinstance(properties, dict) else None
    if (not isinstance(schema, dict) or schema.get("type") != "object"
            or not set(schema).issubset({"type", "properties", "required", "$schema"})
            or schema.get("required") != ["agents"] or not isinstance(properties, dict) or set(properties) != {"agents"}
            or not isinstance(field, dict) or field.get("type") != "string"
            or not set(field).issubset({"type", "title", "description", "enum", "enumNames", "default"})
            or field.get("enum") != ["2", "3", "4"] or field.get("default") != "2"
            or field.get("enumNames") != ["2 agents (default)", "3 agents", "4 agents"]):
        raise ValueError("The native DeLM selector schema changed")
    return {"id": request_id, "result": {"action": "accept", "content": {"agents": str(agents)}}}


def inspect_run(run, evidence_root):
    saved = json.loads((run / "run.json").read_text())
    workers = saved.get("workers", [])
    result = {"run_id": run.name, "status": saved["status"],
              "task": saved["request"]["task"], "model": saved["request"]["model"],
              "reasoning_effort": saved["request"]["reasoning_effort"],
              "worker_count": saved["request"].get("worker_count", 2),
              "worker_threads": [worker.get("thread") for worker in workers],
              "worker_paths": saved["workspace"]["workers"],
              "capability_report": saved["request"].get("auth_settings", {}).get("capability_report")}
    delivery = run / "workspace/delivery/result.json"
    if delivery.exists():
        result["delivery"] = json.loads(delivery.read_text())
    temporary_paths = [*saved["workspace"]["workers"]]
    if saved["workspace"].get("baseline"):
        temporary_paths.append(saved["workspace"]["baseline"])
    result["temporary_workspaces_removed"] = all(not Path(path).exists() for path in temporary_paths)
    database = run / "board/board.sqlite3"
    if database.exists():
        # Apple's SQLite cannot reopen this WAL-mode database read-only once
        # native shutdown removes its -shm/-wal files. Inspect a private copy,
        # including any remaining WAL, after observed native writers stop.
        watchdogs = [json.loads(path.read_text()) for path in run.glob("launches/*/watchdog.json")]
        if any(identity_running(watchdog[key]) for watchdog in watchdogs for key in ["host", "runtime"]):
            raise RuntimeError("Cannot snapshot board while its native writers are running")
        copied = evidence_root / f"board-{run.name}" / "board.sqlite3"
        copied.parent.mkdir()
        for suffix in ["", "-wal"]:
            source = database.with_name(database.name + suffix)
            if source.exists():
                shutil.copyfile(source, copied.with_name(copied.name + suffix))
        connection = sqlite3.connect(copied)
        try:
            result["board_events"] = [{"sequence": seq, "worker": worker, "kind": kind,
                                        "body": json.loads(body)}
                                       for seq, worker, kind, body in connection.execute(
                                           "SELECT seq,worker,kind,body FROM events ORDER BY seq")]
        finally:
            connection.close()
        result["board_evidence"] = str(copied)
        result["peer_import_count"] = sum(event["kind"] == "apply" for event in result["board_events"])
    for name in ["completion.json", "status.json"]:
        path = run / name
        if path.exists():
            result[name] = json.loads(path.read_text())
    result["shutdown_reports"] = [json.loads(path.read_text())
                                  for path in run.glob("launches/*/shutdown-report.json")]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--auth-home", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, default=SOURCE / "target/debug/delm")
    parser.add_argument("--codex", default=shutil.which("codex"))
    parser.add_argument("--model", required=True)
    parser.add_argument("--effort", required=True, choices=["none", "minimal", "low", "medium", "high", "xhigh"])
    parser.add_argument("--agents", type=int, choices=[2, 3, 4], default=2,
                        help="Explicit qualification choice replayed through the native selector (default: 2)")
    parser.add_argument("--service-tier", default="default")
    parser.add_argument("--task-file", type=Path, required=True)
    parser.add_argument("--timeout-seconds", type=int, default=1100)
    args = parser.parse_args()
    if not args.codex or not shutil.which(args.codex):
        parser.error("--codex must name an existing executable")
    if not args.runtime.is_file() or not os.access(args.runtime, os.X_OK):
        parser.error("--runtime must name an existing executable build")
    if not 60 <= args.timeout_seconds <= 1800:
        parser.error("--timeout-seconds must be between 60 and 1800")
    if not args.model.strip() or any(character.isspace() for character in args.model):
        parser.error("--model must be a nonempty native model name without whitespace")
    auth = args.auth_home.resolve() / "auth.json"
    if not auth.is_file():
        parser.error("--auth-home must contain an existing file-backed native login")
    try:
        task = read_task(args.task_file)
    except (OSError, UnicodeError) as error:
        parser.error(f"Cannot read --task-file as UTF-8: {error}")
    if not task.startswith("$delm:run ") or not task.removeprefix("$delm:run ").strip():
        parser.error("--task-file must begin with '$delm:run ' followed by a nonempty task")
    if "\x00" in task:
        parser.error("--task-file must not contain NUL bytes")
    root = args.out.resolve()
    if root.exists() or root.is_symlink():
        parser.error("--out must name a new qualification directory")
    if root.is_relative_to(args.auth_home.resolve()):
        parser.error("--out must be outside the existing native login/configuration home")
    root.mkdir(parents=True, exist_ok=False)
    root.chmod(0o700)
    home, project = root / "home", root / "project"
    home.mkdir(mode=0o700)
    project.mkdir()
    node = shutil.which("node")
    if node:
        node_root = Path(node).resolve().parent.parent
        toolchain = home / ".nvm/versions/node"
        toolchain.parent.mkdir(parents=True)
        toolchain.symlink_to(node_root)
    environment = {"PATH": os.environ["PATH"], "HOME": str(home), "CODEX_HOME": str(home),
                   "SHELL": "/bin/zsh", "LANG": "en_US.UTF-8", "TMPDIR": str(root)}
    git_environment = dict(environment, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null")
    def git(*arguments):
        # No detached maintenance may change .git after the baseline snapshot.
        subprocess.run(["/usr/bin/git", "-c", "maintenance.auto=false", "-c", "gc.auto=0",
                        "-C", str(project), *arguments],
                       env=git_environment, check=True, capture_output=True)
    git("init", "--quiet", "--template=")
    (project / "README.md").write_text("# Fresh-install qualification project\n\nImplement the requested task here.\n")
    git("add", "README.md")
    git("-c", "user.name=DeLM Qualification", "-c", "user.email=qualification@localhost",
        "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "commit", "-qm", "Fixture")
    before = snapshot(project)
    source_identity = (project.stat().st_dev, project.stat().st_ino)
    save(root / "source-before.json", before)
    (root / "request.txt").write_text(task)
    config = (f"model = {json.dumps(args.model)}\nmodel_reasoning_effort = {json.dumps(args.effort)}\n"
              f"service_tier = {json.dumps(args.service_tier)}\n"
              'model_provider = "openai"\ncli_auth_credentials_store = "file"\n'
              'approval_policy = "on-request"\nsandbox_mode = "danger-full-access"\n'
              'web_search = "live"\nallow_login_shell = false\n'
              '[features]\nplugins = true\nhooks = true\nmemories = false\n'
              'shell_snapshot = false\nmulti_agent = false\nmulti_agent_v2 = false\n'
              'unified_exec = true\ncode_mode = false\nview_image = true\n')
    (home / "config.toml").write_text(config)
    marketplace = root / "marketplace"
    stage_package(SOURCE, args.runtime.resolve(), marketplace / "plugin")
    catalog = marketplace / ".agents/plugins"
    catalog.mkdir(parents=True)
    save(catalog / "marketplace.json", {"name": "delm-local", "plugins": [{"name": "delm",
         "source": {"source": "local", "path": "./plugin"}}]})
    evidence = {"qualification": "Fresh plugin/config installation reusing existing native login",
                "helper_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                "host_version": subprocess.check_output([args.codex, "--version"], text=True).strip(),
                "runtime_sha256": hashlib.sha256(args.runtime.read_bytes()).hexdigest(),
                "model": args.model, "reasoning_effort": args.effort, "agents": args.agents, "status": "preparing",
                "new_account_login_qualified": False, "source_identity": source_identity}
    clients = []
    owner = None
    thread = turn = None
    started = time.monotonic()
    runs_root = home / "Library/Application Support/DeLM/runs"
    try:
        # This reference is the only auth operation performed by the helper.
        # Native Codex owns normal loading/refreshing of the existing account store.
        (home / "auth.json").symlink_to(auth)
        installs = []
        for command in [["marketplace", "add", str(marketplace)], ["add", "delm@delm-local"]]:
            result = subprocess.run([args.codex, "plugin", *command, "--json"], cwd=root,
                                    env=environment, text=True, capture_output=True, check=True, timeout=60)
            installs.append(json.loads(result.stdout))
        evidence["native_installation"] = installs
        evidence["installation_elapsed_seconds"] = round(time.monotonic() - started, 3)
        discovery = EvidenceRPC(args.codex, home, root, "discovery")
        clients.append(discovery)
        listing = discovery.request("hooks/list", {"cwds": [str(project)]})
        evidence["untrusted_hooks"] = listing
        hooks = qualification_hooks(listing, "untrusted")
        config = (home / "config.toml").read_text()
        for hook in hooks:
            config += f"\n[hooks.state.{json.dumps(hook['key'])}]\ntrusted_hash = {json.dumps(hook['currentHash'])}\n"
        discovery.close()
        (home / "config.toml").write_text(config)
        owner = EvidenceRPC(args.codex, home, root, "parent")
        clients.append(owner)
        evidence["trusted_hooks"] = owner.request("hooks/list", {"cwds": [str(project)]})
        qualification_hooks(evidence["trusted_hooks"], "trusted")
        account = owner.request("account/read", {"refreshToken": False})
        assert account.get("account", {}).get("type") == "chatgpt", "Native account was not available"
        evidence["native_login_reused"] = True
        skills = owner.request("skills/list", {"cwds": [str(project)], "forceReload": True})
        evidence["skill_discovery"] = skills
        selected = [skill for entry in skills["data"] for skill in entry["skills"]
                    if skill.get("pluginId") == "delm@delm-local" and skill["enabled"]]
        assert len(selected) == 1
        created = owner.request("thread/start", {"cwd": str(project), "model": args.model,
                "modelProvider": "openai", "approvalPolicy": "on-request", "sandbox": "danger-full-access",
                "ephemeral": False,
                "developerInstructions": f"This is an authorized isolated qualification. The user task applies only to {project}. Use the installed DeLM skill and native invocation capture. Deliver the assembled source into this original project and perform only necessary focused follow-up checks there. Do not stage or commit changes. Do not modify any existing repository, user configuration, installed plugin, or browser profile outside {root}; do not read credentials. Native Codex alone may use its configured login store."})
        thread = created["thread"]["id"]
        evidence["parent_thread"] = thread
        start = owner.request("turn/start", {"threadId": thread, "input": [
            {"type": "text", "text": task},
            {"type": "skill", "name": selected[0]["name"], "path": selected[0]["path"]}]})
        turn = start["turn"]["id"]
        evidence["parent_turn"] = turn
        evidence["parent_start_elapsed_seconds"] = round(time.monotonic() - started, 3)
        evidence["status"] = "running"
        save(root / "result.json", evidence)
        last_summary = None
        observed_messages, selector_id = 0, None
        while time.monotonic() - started < args.timeout_seconds:
            messages = owner.messages[observed_messages:]
            observed_messages += len(messages)
            for message in messages:
                if message.get("method") == "mcpServer/elicitation/request":
                    if selector_id is not None and message.get("id") == selector_id:
                        continue
                    if selector_id is not None:
                        raise RuntimeError("Qualification received more than one selector request")
                    response = selector_response(message, args.agents, thread, turn)
                    assert not list(runs_root.glob("*/run.json")), "A run started before agent-count confirmation"
                    selector_id = message["id"]
                    owner.send(response)
                    evidence["agent_selection"] = {"request": message, "response": response,
                        "elapsed_seconds": round(time.monotonic() - started, 3)}
            runs = list(runs_root.glob("*/run.json"))
            if runs:
                assert len(runs) == 1, "Qualification launched more than one DeLM run"
                current = json.loads(runs[0].read_text())
                summary = (current["status"], tuple(worker.get("thread") for worker in current["workers"]))
                if summary != last_summary:
                    progress = {"phase": summary[0], "workers": len([x for x in summary[1] if x]),
                                "elapsed_seconds": round(time.monotonic() - started, 1)}
                    evidence.setdefault("progress", []).append(progress)
                    print(json.dumps(progress), flush=True)
                    last_summary = summary
            done = next((message for message in owner.messages if message.get("method") == "turn/completed"
                         and message.get("params", {}).get("turn", {}).get("id") == turn), None)
            if done:
                evidence["parent_completion"] = done
                break
            if owner.process.poll() is not None:
                raise RuntimeError("Native parent exited before its turn completed")
            time.sleep(0.25)
        else:
            raise TimeoutError("Fresh-install qualification exceeded its bound")
        runs = list(runs_root.glob("*/run.json"))
        evidence["runs"] = [inspect_run(path.parent, root) for path in runs]
        evidence["source_unchanged"] = snapshot(project) == before
        evidence["git_state_preserved"] = git_state(snapshot(project)) == git_state(before)
        assert evidence["git_state_preserved"], "Original Git administration or staging changed"
        assert (project.stat().st_dev, project.stat().st_ino) == source_identity, "Original project identity changed"
        assert len(evidence["runs"]) == 1, "Expected exactly one native DeLM invocation"
        assert selector_id is not None, "The mandatory native selector was not observed"
        run = evidence["runs"][0]
        assert run["worker_count"] == args.agents and len(set(run["worker_threads"])) == args.agents and all(run["worker_threads"]), "Native worker roster did not match the selected count"
        assert run["model"] == args.model and run["reasoning_effort"] == args.effort, "Worker model selection changed"
        registration = Path("/tmp").resolve() / f"delm-{os.getuid()}/lifecycle/session-{thread}.json"
        evidence["lifecycle_binding"] = json.loads(registration.read_text())
        binding = evidence["lifecycle_binding"]
        assert binding["consumed"] and binding["run_id"] == run["run_id"], "Native launch was not bound to the run"
        assert binding["binding"]["session_id"] == thread and binding["binding"]["turn_id"] == turn, "Native launch was bound to a different parent"
        evidence.update(task_handoff(task.removeprefix("$delm:run "), run["task"]))
        assert evidence["task_content_unchanged"], "Task content changed beyond a terminal newline"
        assert run["status"] in ["complete", "delivered"], "DeLM did not deliver its result"
        delivery = run.get("delivery", {})
        assert delivery.get("delivered") is True and Path(delivery["project"]) == project, "Result was not delivered to the original project"
        assert delivery.get("cleanup_complete") is True and run["temporary_workspaces_removed"], "Temporary workspaces were not cleaned"
        assert all(report["ownership_resolved"] and not report["survivors"] and not report["errors"]
                   for report in run["shutdown_reports"]) and run["shutdown_reports"], "Shutdown was not verified"
        evidence["delivery_qualified"] = True
        evidence["parent_followup_verification_required"] = delivery.get("verification_required", False)
        # Do not infer a successful relocated check merely from the parent's
        # final message. Preserve this distinct outcome for manual evidence review.
        evidence["status"] = "delivered_requires_verification" if delivery.get("verification_required") else "passed"
    except Exception as error:
        evidence["status"] = "failed"
        evidence["failure"] = str(error)
        if owner and thread and turn and owner.process.poll() is None:
            try:
                owner.request("turn/interrupt", {"threadId": thread, "turnId": turn}, timeout=10)
            except Exception as stop_error:
                evidence["interrupt_error"] = str(stop_error)
        raise
    finally:
        for client in clients:
            try:
                client.close()
            except Exception as close_error:
                evidence.setdefault("client_close_errors", []).append(str(close_error))
        try:
            evidence["auth_reference_removed"] = remove_auth_reference(home, runs_root, clients)
        except Exception as cleanup_error:
            evidence["auth_reference_removed"] = False
            evidence["cleanup_error"] = str(cleanup_error)
        # No recursive home inventory or archive: auth-store reference is excluded.
        for index, client in enumerate(clients):
            save(root / f"native-{index}-requests.json", client.outgoing)
            save(root / f"native-{index}-messages.json", client.messages)
        if thread:
            registration = Path("/tmp").resolve() / f"delm-{os.getuid()}/lifecycle/session-{thread}.json"
            if registration.exists():
                evidence["lifecycle_binding"] = json.loads(registration.read_text())
        evidence["elapsed_seconds"] = round(time.monotonic() - started, 3)
        evidence["source_unchanged"] = snapshot(project) == before
        evidence["git_state_preserved"] = git_state(snapshot(project)) == git_state(before)
        evidence["source_identity_unchanged"] = (project.stat().st_dev, project.stat().st_ino) == source_identity
        finalization_errors = []
        if not evidence["git_state_preserved"] or not evidence["source_identity_unchanged"]:
            finalization_errors.append("Original project identity or Git administration changed")
        if evidence.get("client_close_errors"):
            finalization_errors.append("Native client shutdown failed")
        if not evidence["auth_reference_removed"]:
            evidence["cleanup_failure"] = "Native processes remained; auth reference retained pending verified shutdown"
            finalization_errors.append(evidence["cleanup_failure"])
        if finalization_errors:
            evidence["status"] = "failed"
            evidence["finalization_errors"] = finalization_errors
        save(root / "result.json", evidence)
        print(json.dumps({"status": evidence["status"], "elapsed_seconds": evidence["elapsed_seconds"],
                          "failure": evidence.get("failure"), "evidence": str(root / "result.json")}), flush=True)
        if finalization_errors:
            raise RuntimeError("; ".join(finalization_errors))


if __name__ == "__main__":
    main()
