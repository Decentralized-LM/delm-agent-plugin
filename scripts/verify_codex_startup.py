#!/usr/bin/env python3
"""Qualify the production Codex plugin in the real terminal in under one minute.

The default scripted case uses a loopback Responses provider, never an account
or model. It exercises the installed plugin, invocation hook, metadata lookup,
two native worker forks, native commands, shared publication, and delivery.
The failure case checks native provider failure and continued ordinary chat.
Only --case live uses an explicitly supplied existing login, with Astra medium.
"""

import argparse
import ctypes
import errno
import fcntl
import gzip
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import pty
import re
import select
import shlex
import shutil
import signal
import struct
import subprocess
import sys
import termios
import threading
import time

from build import PACKAGE_FILES, payload_digest, stage_package
from install_support import package_files
from verify_fresh_install import git_state, identity_running, redacted, snapshot
from verify_native_lifecycle import RPC

SOURCE = Path(__file__).resolve().parent.parent
STARTUP_CASES = ("scripted", "failure")
HELLO = b"Hello from DeLM.\n"
FAILURE_MARKER = "DELM_NATIVE_STARTUP_FAILURE_FIXTURE"
ORDINARY_PROMPT = "Reply with exactly: Ordinary conversation is ready."
ORDINARY_REPLY = "Native conversation resumed successfully."
TASK = ('$delm:run Create hello.txt containing exactly "Hello from DeLM." followed by a newline. '
        'This is the whole task. One agent should do the focused file-content check once. '
        'Finish immediately after publishing and delivering the file. Do not add tests or other files. '
        'Do not read credentials or modify anything outside this project and DeLM temporary workspaces.')


def startup_source_digest(source):
    """Bind production source, host resources and every imported harness helper."""
    source = Path(source)
    names = set(PACKAGE_FILES) | {
        "Cargo.toml", "Cargo.lock", "plugin/worker.md", "scripts/verify_codex_startup.py",
        "scripts/build.py", "scripts/install_support.py", "scripts/verify_fresh_install.py",
        "scripts/verify_native_lifecycle.py", "scripts/maintenance.py",
        "scripts/dependency_notices.py",
    }
    names.update(str(path.relative_to(source)) for path in (source / "src").rglob("*.rs"))
    digest = hashlib.sha256()
    for name in sorted(names):
        digest.update(name.encode() + b"\0" + (source / name).read_bytes() + b"\0")
    return digest.hexdigest()


def read_json(path):
    try:
        return json.loads(path.read_text())
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def write_json(path, value):
    path.write_text(json.dumps(redacted(value), indent=2) + "\n")


def classify_request(payload):
    schema = payload.get("text", {}).get("format", {}).get("schema", {})
    if set(schema.get("properties", {})) == {"title"}:
        return "title", 0
    workers = set(re.findall(r"You are worker ([12])\.", json.dumps(payload)))
    if len(workers) > 1:
        raise RuntimeError("Ambiguous scripted native worker identity")
    return ("worker", int(next(iter(workers)))) if workers else ("parent", 0)


def tool_name(payload, suffix):
    def names(value):
        if isinstance(value, dict):
            if isinstance(value.get("name"), str):
                yield value["name"]
            for item in value.values():
                yield from names(item)
        elif isinstance(value, list):
            for item in value:
                yield from names(item)
    inventory = [payload.get("tools", [])] + [item.get("tools", []) for item in payload.get("input", [])
        if isinstance(item, dict) and item.get("type") == "additional_tools"]
    matches = {name for name in names(inventory)
               if name == suffix or name.endswith("__" + suffix)}
    if len(matches) != 1:
        raise RuntimeError(f"Expected one native {suffix} tool, found {sorted(matches)}; inventory: {sorted(set(names(inventory)))}")
    return matches.pop()


def native_call(payload, suffix, arguments, call_id):
    """Use the native inventory, including Astra's code execution surface."""
    try:
        name = tool_name(payload, suffix)
        return {"type": "function_call", "call_id": call_id,
                "name": name, "arguments": json.dumps(arguments)}
    except RuntimeError:
        executor = tool_name(payload, "exec")
        kind, worker = classify_request(payload)
        if suffix.startswith("delm_") and kind == "worker":
            expected = f"mcp__delm_coordination_{worker}__{suffix}"
            return {"type": "custom_tool_call", "call_id": call_id, "name": executor,
                "input": "const found=ALL_TOOLS.filter(t=>t.name===" + json.dumps(expected) + ");"
                    + "if(found.length!==1)throw Error('Expected own native coordination tool');"
                    + "text(await tools[found[0].name](" + json.dumps(arguments) + "));"}
        descriptions = json.dumps(payload.get("input", []) + payload.get("tools", []))
        matches = set(re.findall(r"\b(?:mcp__[A-Za-z0-9_]+__)?" + re.escape(suffix) + r"(?=\()", descriptions))
        if len(matches) != 1:
            raise RuntimeError(f"Native code tool declaration missing or ambiguous: {suffix}: {sorted(matches)}")
        return {"type": "custom_tool_call", "call_id": call_id, "name": executor,
                "input": "text(await tools." + matches.pop() + "(" + json.dumps(arguments) + "));"}


def fixture_approval(details, saved):
    """The fixture user allows only this run's native DeLM coordination tools."""
    worker = details.get("worker")
    if type(worker) is not int or worker not in (1, 2):
        return False
    request = details.get("request", {})
    peers = saved.get("workers", [])
    if len(peers) != 2:
        return False
    peer = peers[worker - 1]
    server = f"delm_coordination_{worker}"
    allowed = {"delm_read", "delm_status", "delm_list", "delm_expand", "delm_publish", "delm_apply",
        "delm_complete", "delm_task_create", "delm_task_claim", "delm_task_finish", "delm_task_release",
        "delm_task_split", "delm_task_update", "delm_check_begin", "delm_check_finish"}
    return (details.get("method") == "mcpServer/elicitation/request"
        and request.get("serverName") == server
        and request.get("threadId") == peer.get("thread") and bool(peer.get("thread"))
        and request.get("turnId") == peer.get("turn") and bool(peer.get("turn"))
        and request.get("mode") == "form"
        and request.get("_meta", {}).get("codex_approval_kind") == "mcp_tool_call"
        and request.get("requestedSchema") == {"type": "object", "properties": {}}
        and request.get("message") in {f'Allow the {server} MCP server to run tool "{tool}"?' for tool in allowed})


def publication_evidence(run):
    """Require successful native tool results; prompt/tool-name text is not evidence."""
    workers = set()
    path = run / "events.jsonl"
    if not path.is_file():
        return False
    for line in path.read_text().splitlines():
        event = json.loads(line)
        data = event.get("data", {})
        item = data.get("params", {}).get("item", {})
        if (event.get("kind") != "native" or data.get("method") != "item/completed"
                or item.get("type") != "mcpToolCall" or item.get("tool") != "delm_publish"
                or item.get("status") != "completed" or item.get("error") is not None):
            continue
        for worker in (1, 2):
            if item.get("server") != f"delm_coordination_{worker}":
                continue
            if worker == 1 and item.get("arguments", {}).get("paths") != ["hello.txt"]:
                continue
            for content in item.get("result", {}).get("content", []):
                try:
                    result = json.loads(content.get("text", "")).get("result", {})
                except json.JSONDecodeError:
                    continue
                if type(result.get("publication_id")) is int and result["publication_id"] > 0:
                    workers.add(worker)
    return workers == {1, 2}


def invocation_records(project):
    lifecycle = Path("/tmp").resolve() / f"delm-{os.getuid()}" / "lifecycle"
    for path in lifecycle.glob("input-*.json"):
        value = read_json(path) or {}
        if value.get("project") == str(project):
            yield path, value


def run_records(runs):
    return [(path, value) for path in runs.glob("*/run.json") if (value := read_json(path))]


def owned_identities(runs, project):
    identities = {}
    for path in runs.glob("*/launches/*/watchdog.json"):
        value = read_json(path) or {}
        for key in ("host", "runtime"):
            if item := value.get(key):
                identities[item["pid"]] = item
    for path in runs.glob("*/launches/*/shutdown-report.json"):
        for item in (read_json(path) or {}).get("owned_processes", []):
            identities[item["pid"]] = item
    for path, _ in invocation_records(project):
        if item := (read_json(path.with_suffix(".launch.json")) or {}).get("process"):
            identities[item["pid"]] = item
    return list(identities.values())


def process_identity(pid):
    """Darwin birth identity for a directly owned process or its observed child."""
    class Info(ctypes.Structure):
        _fields_ = [(key, ctypes.c_uint32) for key in (
            "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid", "rgid",
            "svuid", "svgid", "reserved")] + [("comm", ctypes.c_char * 16),
            ("name", ctypes.c_char * 32)] + [(key, ctypes.c_uint32) for key in (
                "nfiles", "pgid", "pjobc", "tdev", "tpgid")] + [("nice", ctypes.c_int32),
                ("started_seconds", ctypes.c_uint64), ("started_micros", ctypes.c_uint64)]
    info = Info()
    library = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
    library.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                     ctypes.c_void_p, ctypes.c_int]
    library.proc_pidinfo.restype = ctypes.c_int
    count = library.proc_pidinfo(pid, 3, 0, ctypes.byref(info), ctypes.sizeof(info))
    if count == 0 and ctypes.get_errno() in (errno.ESRCH, errno.ENOENT):
        return None
    if count != ctypes.sizeof(info):
        raise RuntimeError("Could not inspect an owned process identity")
    if info.uid != os.getuid():
        raise RuntimeError("Qualification process changed owner")
    return {key: int(getattr(info, key)) for key in (
        "pid", "started_seconds", "started_micros", "uid")}


def stop_process_group(process, timeout):
    """Reap owned children; an already-exited group is a successful shutdown."""
    if process.poll() is not None:
        return
    for sig in (signal.SIGTERM, signal.SIGKILL):
        if process.poll() is not None:
            return
        try:
            # Codex may change its process group while initializing its TUI.
            if os.getpgid(process.pid) == process.pid:
                os.killpg(process.pid, sig)
            else:
                process.send_signal(sig)
        except ProcessLookupError:
            pass
        except PermissionError:
            if process.poll() is None:
                process.send_signal(sig)
        try:
            process.wait(timeout=timeout())
            return
        except subprocess.TimeoutExpired:
            pass
    raise RuntimeError("Owned native process did not exit")


class ScriptedProvider:
    """Native host executes every tool; this provider supplies deterministic choices."""
    def __init__(self, case, root, home, project, runs, started):
        self.case, self.root, self.home = case, root, home
        self.project, self.runs, self.started = project, runs, started
        self.requests, self.errors = [], []
        self.turns, self.observed_workers = {}, set()
        self.stop = threading.Event()
        self.peer_ready = threading.Event()
        self.ordinary_seen = threading.Event()
        self.failure_injected = False
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                try:
                    body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                    if self.headers.get("Content-Encoding") == "gzip":
                        body = gzip.decompress(body)
                    payload = json.loads(body)
                    kind, worker = classify_request(payload)
                    owner.requests.append({"kind": kind, "worker": worker,
                        "at_seconds": time.monotonic() - started, "model": payload.get("model"),
                        "path": self.path})
                    if kind == "worker":
                        owner.observed_workers.add(worker)
                    if case == "failure" and kind == "worker":
                        records = run_records(runs)
                        if len(records) != 1 or len({item.get("thread") for item in records[0][1].get("workers", [])
                                                     if item.get("thread")}) != 2:
                            raise RuntimeError("Native failure fixture requires both recorded worker forks")
                        owner.failure_injected = True
                        encoded = json.dumps({"error": {"message": FAILURE_MARKER,
                            "type": "invalid_request_error", "code": "fixture_failure"}}).encode()
                        self.send_response(400)
                        self.send_header("Content-Type", "application/json")
                        self.send_header("Content-Length", str(len(encoded)))
                        self.end_headers()
                        self.wfile.write(encoded)
                        return
                    item = owner.next_item(payload, kind, worker)
                    rid = f"startup-{len(owner.requests)}"
                    events = [{"type": "response.created", "response": {"id": rid}}]
                    if item:
                        events.append({"type": "response.output_item.done", "item": item})
                    events.append({"type": "response.completed", "response": {"id": rid,
                        "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}}})
                    stream = "".join("data: " + json.dumps(value) + "\n\n" for value in events).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(stream)))
                    self.end_headers()
                    self.wfile.write(stream)
                except (BrokenPipeError, ConnectionResetError):
                    pass
                except Exception as error:
                    owner.errors.append(str(error))
                    self.send_error(500)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    @staticmethod
    def message(text):
        return {"type": "message", "id": "startup-message", "role": "assistant",
                "content": [{"type": "output_text", "text": text}]}

    def next_item(self, payload, kind, worker):
        if kind == "title":
            return self.message('{"title":"DeLM startup qualification"}')
        if not worker and ORDINARY_PROMPT in json.dumps(payload.get("input", [])):
            self.ordinary_seen.set()
            return self.message(ORDINARY_REPLY)
        step = self.turns.get(worker, 0)
        self.turns[worker] = step + 1
        if not worker:
            if step == 0:
                capture = next(invocation_records(self.project))[0]
                runtime = next((self.home / "plugins/cache/startup-proof/delm").glob("*/bin/delm"))
                command = shlex.quote(str(runtime)) + " follow --capture " + shlex.quote(str(capture))
                name, arguments = "exec_command", {"cmd": command, "yield_time_ms": 1000,
                                                  "max_output_tokens": 1000}
            else:
                # Keep following the exact native execution handle, as the skill requests.
                outputs = [item.get("output", "") for item in payload.get("input", [])
                           if isinstance(item, dict) and item.get("type") in ("function_call_output", "custom_tool_call_output")]
                text = str(outputs[-1]) if outputs else ""
                handles = re.findall(r"(?:session ID|session_id)[^0-9]{0,8}(\d+)", text, re.I)
                if handles:
                    name, arguments = "write_stdin", {"session_id": int(handles[-1]),
                        "chars": "", "yield_time_ms": 1000, "max_output_tokens": 1000}
                else:
                    return self.message("DeLM finished its native task.")
        elif step == 0:
            code = ('from pathlib import Path; import os; '
                    'assert os.environ["DELM_STARTUP_ENV"] == "native-environment"; ')
            if worker == 1:
                code += f'p=Path("hello.txt"); p.write_bytes({HELLO!r}); assert p.read_bytes()=={HELLO!r}; '
            code += 'print("native-check-ok")'
            name, arguments = "exec_command", {"cmd": shlex.quote(sys.executable) + " -c " + shlex.quote(code),
                "yield_time_ms": 1000, "max_output_tokens": 1000}
        elif step == 1:
            name, arguments = "delm_publish", {"idempotency_key": f"startup-publish-{worker}",
                "summary": "Checked hello.txt" if worker == 1 else "Native environment check passed",
                "paths": ["hello.txt"] if worker == 1 else []}
        elif step == 2:
            if worker == 1 and not self.peer_ready.wait(5):
                raise RuntimeError("Second native worker did not publish its contribution")
            if worker == 2:
                self.peer_ready.set()
            name, arguments = "delm_complete", {"idempotency_key": f"startup-complete-{worker}",
                "expected_revision": 1, "outcome": "complete" if worker == 1 else "waiting",
                "summary": "Native scripted qualification complete", "checks": [], "artifacts": []}
            if worker == 2:
                arguments["dependency"] = "worker:1"
        else:
            return self.message("Contribution complete.")
        return native_call(payload, name, arguments, f"startup-worker-{worker}-step-{step}")

    def close(self):
        self.stop.set()
        self.peer_ready.set()
        self.server.shutdown()
        self.server.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, default=SOURCE / "target/debug/delm")
    parser.add_argument("--codex", default=shutil.which("codex"))
    parser.add_argument("--case", choices=(*STARTUP_CASES, "live"), default="scripted")
    parser.add_argument("--auth-home", type=Path)
    args = parser.parse_args()
    if sys.platform != "darwin" or not args.codex or not args.runtime.is_file():
        parser.error("Native macOS, installed Codex and a built --runtime are required")
    if args.case == "live" and (not args.auth_home or not (args.auth_home / "auth.json").is_file()):
        parser.error("--case live requires --auth-home with an existing file-backed login")
    root = args.out.resolve()
    if root.exists() or (args.auth_home and root.is_relative_to(args.auth_home.resolve())):
        parser.error("--out must be a new directory outside the existing account home")
    root.mkdir(parents=True, mode=0o700)
    home, project = root / "home", root / "project"
    home.mkdir(mode=0o700)
    project.mkdir()
    runs = home / "Library/Application Support/DeLM/runs"
    started = time.monotonic()
    deadline = started + 54
    codex_path = Path(shutil.which(args.codex) or args.codex).absolute()
    environment = {"PATH": str(codex_path.parent) + os.pathsep + os.environ["PATH"], "HOME": str(home), "CODEX_HOME": str(home),
        "SHELL": "/bin/zsh", "LANG": "en_US.UTF-8", "TERM": "xterm-256color", "NO_COLOR": "1",
        "TMPDIR": os.environ.get("TMPDIR", "/tmp"), "DELM_STARTUP_ENV": "native-environment"}
    evidence = {"kind": "native-codex-startup", "schema_version": 1, "case": args.case,
        "passed": False, "architecture": platform.machine(), "worker_count": 2,
        "model_calls": 0 if args.case != "live" else None,
        "runtime_sha256": hashlib.sha256(args.runtime.read_bytes()).hexdigest(),
        "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "source_digest": startup_source_digest(SOURCE), "cleanup_errors": []}
    raw, clients = bytearray(), []
    child = provider = master = None
    initial_git = {}
    direct_identities = []
    answered_approvals = set()
    evidence["fixture_approvals"] = []

    def remaining(limit=3):
        return max(.01, min(limit, deadline - time.monotonic()))

    def hard_deadline(_signal, _frame):
        raise TimeoutError("Startup qualification reached its bounded work deadline")

    signal.signal(signal.SIGALRM, hard_deadline)
    signal.setitimer(signal.ITIMER_REAL, 51)
    try:
        evidence["host_version"] = subprocess.check_output([args.codex, "--version"],
            text=True, timeout=remaining()).strip()
        subprocess.run(["/usr/bin/git", "init", "-q", "--template=", str(project)],
            env=dict(environment, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null"),
            check=True, timeout=remaining())
        initial_git = git_state(snapshot(project))
        config = ('model = "gpt-6-astra"\nmodel_reasoning_effort = "medium"\n'
            'approval_policy = "on-request"\nsandbox_mode = "danger-full-access"\n'
            'cli_auth_credentials_store = "file"\nweb_search = "disabled"\n'
            'allow_login_shell = false\ncheck_for_update_on_startup = false\n')
        if args.case != "live":
            provider = ScriptedProvider(args.case, root, home, project, runs, started)
            config += ('model_provider = "fixture"\n[model_providers.fixture]\nname = "Scripted native qualification"\n'
                f'base_url = "http://127.0.0.1:{provider.server.server_port}/v1"\nwire_api = "responses"\n'
                'requires_openai_auth = false\nrequest_max_retries = 0\nstream_max_retries = 0\n')
        config += ('[features]\nplugins = true\nhooks = true\nmemories = false\nshell_snapshot = false\n'
            'multi_agent = false\nmulti_agent_v2 = false\nunified_exec = true\ncode_mode = false\napps = false\n'
            f'[projects.{json.dumps(str(project))}]\ntrust_level = "trusted"\n')
        (home / "config.toml").write_text(config)
        if args.case == "live":
            (home / "auth.json").symlink_to(args.auth_home.resolve() / "auth.json")
        package = root / "marketplace/plugin"
        stage_package(SOURCE, args.runtime.resolve(), package)
        expected_package = package_files(package)
        evidence["package_payload_sha256"] = payload_digest(package)
        catalog = root / "marketplace/.agents/plugins"
        catalog.mkdir(parents=True)
        write_json(catalog / "marketplace.json", {"name": "startup-proof", "plugins": [
            {"name": "delm", "source": {"source": "local", "path": "./plugin"}}]})
        for command in (["marketplace", "add", str(root / "marketplace")], ["add", "delm@startup-proof"]):
            subprocess.run([args.codex, "plugin", *command], cwd=root, env=environment,
                capture_output=True, text=True, check=True, timeout=remaining(5))
        installed = list((home / "plugins/cache/startup-proof/delm").glob("*/bin/delm"))
        assert len(installed) == 1 and package_files(installed[0].parent.parent) == expected_package
        evidence["installed_package_matches"] = True
        discovery = RPC(args.codex, home, root, "discovery")
        clients.append(discovery)
        listing = discovery.request("hooks/list", {"cwds": [str(project)]}, timeout=remaining(5))
        hooks = [hook for row in listing["data"] for hook in row["hooks"]
                 if hook.get("pluginId") == "delm@startup-proof"]
        if not hooks:  # Native versions expose plugin attribution through each source instead.
            hooks = [hook for row in listing["data"] for hook in row["hooks"]
                     if "startup-proof/delm/" in json.dumps(hook)]
        assert len(hooks) == 4 and all(hook["handlerType"] == "command" for hook in hooks)
        write_json(root / "hooks-listing.json", listing)
        discovery.close()
        config = (home / "config.toml").read_text()
        for hook in hooks:
            config += f'\n[hooks.state.{json.dumps(hook["key"])}]\ntrusted_hash = {json.dumps(hook["currentHash"])}\n'
        (home / "config.toml").write_text(config)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 115, 0, 0))
        child = subprocess.Popen([args.codex, "--no-daemon", "--no-alt-screen", "-C", str(project), TASK],
            stdin=slave, stdout=slave, stderr=slave, cwd=root, env=environment, start_new_session=True)
        os.close(slave)
        if identity := process_identity(child.pid):
            direct_identities.append(identity)
        ordinary_sent = False
        ordinary_submit_at = None
        while time.monotonic() - started < 50:
            if select.select([master], [], [], .03)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                raw.extend(data)
                if b"\x1b[6n" in data:
                    os.write(master, b"\x1b[1;1R")
                if b"\x1b[c" in data:
                    os.write(master, b"\x1b[?1;2c")
            if provider and provider.errors:
                raise RuntimeError("Scripted provider: " + "; ".join(provider.errors))
            if ordinary_submit_at is not None and time.monotonic() >= ordinary_submit_at:
                os.write(master, b"\r")
                ordinary_submit_at = None
            records = run_records(runs)
            for path, saved in records:
                current = read_json(path.parent / "runtime-status.json") or {}
                for request_id, details in current.get("approvals", {}).items():
                    if request_id in answered_approvals or not fixture_approval(details, saved):
                        continue
                    response_file = root / "fixture-approval.json"
                    write_json(response_file, {"action": "accept", "content": {}})
                    answered_approvals.add(request_id)
                    response = subprocess.run([str(args.runtime.resolve()), "respond", "--run-id", path.parent.name,
                        "--request-id", request_id, "--response-file", str(response_file)], env=environment,
                        capture_output=True, text=True, timeout=remaining(2))
                    if response.returncode:
                        raise RuntimeError("Fixture coordination approval was rejected: " + response.stderr)
                    evidence["fixture_approvals"].append({"request_id": request_id,
                        "worker": details["worker"], "message": details["request"]["message"]})
            if records and records[-1][1].get("status") in ("complete", "completed", "failed", "stopped", "delivery_conflict"):
                if args.case != "failure":
                    break
                if not ordinary_sent and b"DeLM finished its native task." in raw:
                    os.write(master, ORDINARY_PROMPT.encode())
                    ordinary_submit_at = time.monotonic() + .2
                    ordinary_sent = True
                if provider.ordinary_seen.is_set() and ORDINARY_REPLY.encode() in raw:
                    break
            for path, _ in invocation_records(project):
                event_path = path.with_suffix(".events.jsonl")
                if event_path.exists():
                    errors = [event.get("message") for line in event_path.read_text().splitlines()
                              if (event := json.loads(line)).get("type") == "error"]
                    if errors:
                        evidence["startup_errors"] = errors
            if evidence.get("startup_errors") and not records:
                break
            if child.poll() is not None:
                break
        evidence["native_invocation"] = bool(list(invocation_records(project)))
        records = run_records(runs)
        if len(records) != 1:
            raise RuntimeError(f"Expected exactly one production run, observed {len(records)}")
        path, saved = records[0]
        evidence["run_id"], evidence["status"] = path.parent.name, saved.get("status")
        threads = [worker.get("thread") for worker in saved.get("workers", [])]
        evidence["worker_forks_started"] = len({thread for thread in threads if thread})
        evidence["model"] = saved["request"].get("model")
        evidence["effort"] = saved["request"].get("reasoning_effort")
        evidence["native_host_matches"] = Path(saved["request"]["host_executable"]).resolve() == codex_path.resolve()
        delivery = read_json(path.parent / "workspace/delivery/result.json") or {}
        evidence["result_delivered"] = delivery.get("delivered") is True
        evidence["delivery_cleanup_complete"] = delivery.get("cleanup_complete") is True
        evidence["exact_result"] = (project / "hello.txt").is_file() and (project / "hello.txt").read_bytes() == HELLO
        evidence["original_git_preserved"] = git_state(snapshot(project)) == initial_git
        expected_workers = [path.parent / "workspace" / f"worker-{worker}" for worker in (1, 2)]
        evidence["workspaces_removed"] = (saved["workspace"]["workers"] == [str(worker) for worker in expected_workers]
            and all(not worker.exists() for worker in expected_workers)
            and not (path.parent / "workspace/baseline").exists())
        journal = (path.parent / "events.jsonl").read_text()
        evidence["native_check_passed"] = all(any(item.get("exitCode") == 0 and "native-check-ok" in json.dumps(item)
            for item in worker.get("checks", {}).values()) for worker in saved["workers"])
        evidence["shared_publication_observed"] = publication_evidence(path.parent)
        evidence["completion_observed"] = any(worker.get("outcome", {}).get("outcome") == "complete"
            for worker in saved["workers"] if isinstance(worker.get("outcome"), dict))
        if args.case == "failure":
            evidence["explicit_failure"] = FAILURE_MARKER in journal and provider.failure_injected and provider.observed_workers == {1, 2}
            evidence["ordinary_conversation_usable"] = provider.ordinary_seen.is_set()
            evidence["ordinary_response_rendered"] = ORDINARY_REPLY.encode() in raw
            evidence["original_unchanged"] = sorted(p.name for p in project.iterdir()) == [".git"]
            outcome = (evidence["explicit_failure"] and evidence["ordinary_conversation_usable"]
                       and evidence["ordinary_response_rendered"]
                       and evidence["original_unchanged"] and not evidence["result_delivered"])
        else:
            outcome = evidence["result_delivered"] and evidence["exact_result"] and evidence["delivery_cleanup_complete"]
            if args.case == "scripted":
                outcome &= (evidence["native_check_passed"] and evidence["shared_publication_observed"]
                            and evidence["completion_observed"] and provider.observed_workers == {1, 2})
        evidence["passed"] = bool(outcome and evidence["native_invocation"]
            and len(threads) == 2 and evidence["worker_forks_started"] == 2
            and evidence["original_git_preserved"] and evidence["workspaces_removed"]
            and evidence["native_host_matches"]
            and evidence["model"] == "gpt-6-astra" and evidence["effort"] == "medium")
    except Exception as error:
        evidence["error"] = str(error)
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        if provider:
            provider.peer_ready.set()
        def attempt(label, action):
            try:
                return action()
            except ProcessLookupError:
                return None
            except Exception as error:
                evidence["cleanup_errors"].append(f"{label}: {error}")
                return None
        for path, saved in run_records(runs):
            if saved.get("status") not in ("complete", "completed", "failed", "stopped", "delivery_conflict"):
                attempt("stop qualification run", lambda: subprocess.run(
                    [str(args.runtime.resolve()), "stop", "--run-id", path.parent.name],
                    env=environment, capture_output=True, timeout=remaining(2)))
        if child:
            attempt("stop native terminal", lambda: stop_process_group(child, lambda: remaining(.5)))
        identities = attempt("inspect runtime identities", lambda: owned_identities(runs, project))
        known = identities is not None
        identities = (identities or []) + direct_identities
        def still_running(item):
            return attempt("inspect owned runtime process", lambda: identity_running(item))
        for sig in (signal.SIGTERM, signal.SIGKILL):
            for item in identities:
                def stop_identity(item=item, sig=sig):
                    if identity_running(item):
                        os.kill(item["pid"], sig)
                attempt("stop owned runtime process", stop_identity)
            until = min(deadline, time.monotonic() + .5)
            while time.monotonic() < until and any(still_running(item) is not False for item in identities):
                time.sleep(.02)
        for client in clients:
            attempt("stop discovery", lambda: stop_process_group(client.process, lambda: remaining(.5)))
            attempt("close discovery log", client.log.close)
        evidence["owned_processes_stopped"] = (known and all(still_running(item) is False for item in identities)
            and (not child or child.poll() is not None) and all(client.process.poll() is not None for client in clients))
        auth = home / "auth.json"
        if auth.is_symlink() and evidence["owned_processes_stopped"]:
            attempt("remove temporary login reference", auth.unlink)
        evidence["auth_link_removed"] = not auth.exists() and not auth.is_symlink()
        if master is not None:
            attempt("close terminal", lambda: os.close(master))
        if provider:
            attempt("close scripted provider", provider.close)
            evidence["provider_requests"] = provider.requests
        evidence["elapsed_seconds"] = time.monotonic() - started
        evidence["passed"] &= bool(evidence["owned_processes_stopped"] and evidence["auth_link_removed"]
                                   and not evidence["cleanup_errors"] and evidence["elapsed_seconds"] < 60)
        (root / "terminal.raw").write_bytes(raw)
        text = re.sub(r"\x1b\][^\x07]*(?:\x07|\x1b\\)", "", raw.decode(errors="replace"))
        (root / "terminal.txt").write_text(re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text))
        write_json(root / "result.json", evidence)
        print(json.dumps(redacted(evidence), indent=2))
    return 0 if evidence["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
