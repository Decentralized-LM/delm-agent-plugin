#!/usr/bin/env python3
"""Stock Codex lifecycle probe: disposable HOME, scripted loopback responses, no model.

All command targets are fixture processes created here. No user thread IDs,
credentials, configuration, plugin installation, or UI sessions are touched.
"""
import argparse
import gzip
import hashlib
import http.server
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import threading
import time
import uuid


def wait_for(predicate, timeout=20):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise TimeoutError("fixture condition timed out")


def alive(pid):
    try:
        result = subprocess.run(["/bin/ps", "-p", str(pid), "-o", "stat="], capture_output=True, text=True)
        return result.returncode == 0 and not result.stdout.lstrip().startswith("Z")
    except ProcessLookupError:
        return False


class RPC:
    def __init__(self, codex, home, root, label):
        self.root = root
        self.messages = []
        self.next_id = 0
        self.log = open(root / (label + "-stderr.log"), "w")
        env = {"PATH": os.environ["PATH"], "HOME": str(home), "CODEX_HOME": str(home),
               "SHELL": "/bin/zsh", "LANG": "en_US.UTF-8", "TMPDIR": str(root)}
        self.process = subprocess.Popen([codex, "app-server", "--listen", "stdio://"],
            cwd=root, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.log, text=True, bufsize=1, start_new_session=True)
        def read():
            for line in self.process.stdout:
                try:
                    self.messages.append(json.loads(line))
                except json.JSONDecodeError:
                    self.messages.append({"invalid": line})
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()
        self.request("initialize", {"clientInfo": {"name": "delm_lifecycle_fixture", "version": "0.1"},
                                    "capabilities": {"experimentalApi": True}})
        self.send({"method": "initialized", "params": {}})

    def send(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def begin(self, method, params):
        self.next_id += 1
        self.send({"id": self.next_id, "method": method, "params": params})
        return self.next_id

    def response(self, request, timeout=20):
        return wait_for(lambda: next((m for m in self.messages if m.get("id") == request and "method" not in m), None), timeout)

    def request(self, method, params, timeout=20):
        result = self.response(self.begin(method, params), timeout)
        if "error" in result:
            raise RuntimeError(f"{method}: {result['error']}")
        return result["result"]

    def close(self, hard=False):
        if self.process.poll() is None:
            if hard:
                self.process.kill()
            else:
                self.process.stdin.close()
            try:
                self.process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.log.close()


class ScriptedProvider:
    def __init__(self, command, root):
        self.requests = []
        self.finish = threading.Event()
        self.command = command
        owner = self
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass
            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                if self.headers.get("Content-Encoding") == "gzip":
                    body = gzip.decompress(body)
                data = json.loads(body)
                owner.requests.append({"path": self.path, "body": data})
                index = len(owner.requests)
                if index > 1:
                    owner.finish.wait(120)
                rid = f"fixture-{index}"
                events = [{"type":"response.created","response":{"id":rid}}]
                if index == 1:
                    events.append({"type":"response.output_item.done", "item":{
                        "type":"function_call","call_id":"fixture-command","name":"exec_command",
                        "arguments":json.dumps({"cmd":owner.command,"yield_time_ms":1000,"max_output_tokens":100})}})
                else:
                    events.append({"type":"response.output_item.done","item":{
                        "type":"message","role":"assistant","id":"fixture-final",
                        "content":[{"type":"output_text","text":"Fixture complete; no model inference occurred."}]}})
                events.append({"type":"response.completed","response":{"id":rid,"usage":{
                    "input_tokens":0,"output_tokens":0,"total_tokens":0}}})
                encoded = "".join("data: " + json.dumps(e) + "\n\n" for e in events).encode()
                try:
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(encoded)))
                    self.end_headers()
                    self.wfile.write(encoded)
                except (BrokenPipeError, ConnectionResetError):
                    pass
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1",0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever,daemon=True)
        self.thread.start()

    def close(self):
        self.finish.set()
        self.server.shutdown()
        self.server.server_close()


def main():
    parser = argparse.ArgumentParser(description="No-model qualification of DeLM hooks in disposable stock Codex")
    parser.add_argument("--codex", default=shutil.which("codex"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, default=Path("target/debug/examples/native-lifecycle-fixture"))
    parser.add_argument("--case", choices=["interrupt", "stop", "owner-death", "plugin-remove", "preflight"], default="interrupt")
    parser.add_argument("--idle-seconds", type=float)
    args = parser.parse_args()
    idle = args.idle_seconds if args.idle_seconds is not None else (65 if args.case == "stop" else 2)
    assert args.fixture.is_file(), "Build cargo build --offline --example native-lifecycle-fixture first"
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    home = root / "home"
    home.mkdir()
    project = root / "project"
    project.mkdir()
    package = root / "marketplace/plugin"
    for directory in [package / ".codex-plugin", package / "hooks", package / "bin", root / "marketplace/.agents/plugins"]:
        directory.mkdir(parents=True)
    shutil.copy2(args.fixture, package / "bin/delm")
    shutil.copy2("hooks/hooks.json", package / "hooks/hooks.json")
    (package / ".codex-plugin/plugin.json").write_text(json.dumps({
        "name": "native-lifecycle-fixture", "version": "0.1.0", "description": "No-model lifecycle qualification", "hooks": "./hooks/hooks.json"}))
    (root / "marketplace/.agents/plugins/marketplace.json").write_text(json.dumps({
        "name": "native-lifecycle-fixture", "plugins": [{"name": "native-lifecycle-fixture", "source": {"source": "local", "path": "./plugin"}}]}))
    env = {"PATH": os.environ["PATH"], "HOME": str(home), "CODEX_HOME": str(home), "SHELL": "/bin/zsh"}
    provider = ScriptedProvider("", root)
    config = f'''model = "gpt-5.4"
model_provider = "fixture"
approval_policy = "never"
sandbox_mode = "danger-full-access"
web_search = "disabled"
allow_login_shell = false
[model_providers.fixture]
name = "Scripted fixture, no model"
base_url = "http://127.0.0.1:{provider.server.server_port}/v1"
wire_api = "responses"
requires_openai_auth = false
request_max_retries = 0
stream_max_retries = 0
[features]
plugins = true
hooks = true
memories = false
shell_snapshot = false
multi_agent = false
multi_agent_v2 = false
unified_exec = true
code_mode = false
'''
    (home / "config.toml").write_text(config)
    evidence = {"real_model_calls": 0, "case": args.case,
                "host_version": subprocess.check_output([args.codex, "--version"], text=True).strip(),
                "fixture_sha256": hashlib.sha256(args.fixture.read_bytes()).hexdigest(),
                "lifecycle_source_sha256": hashlib.sha256(Path("src/lifecycle.rs").read_bytes()).hexdigest(),
                "isolated_home": str(home)}
    clients, owned = [], []
    try:
        for command in [["marketplace", "add", str(root / "marketplace")], ["add", "native-lifecycle-fixture@native-lifecycle-fixture"]]:
            subprocess.run([args.codex, "plugin", *command], cwd=root, env=env, text=True, capture_output=True, check=True)
        config = (home / "config.toml").read_text()
        discovery = RPC(args.codex, home, root, "discovery")
        clients.append(discovery)
        listing = discovery.request("hooks/list", {"cwds": [str(root)]})
        evidence["untrusted_hooks"] = listing
        hooks = [hook for item in listing["data"] for hook in item["hooks"]]
        assert len(hooks) == 4 and all(h["trustStatus"] == "untrusted" for h in hooks)
        installed = Path(hooks[0]["sourcePath"]).parent.parent / "bin/delm"
        # Test-only simulation of explicit /hooks review. Never changes a real home.
        for hook in hooks:
            config += f"\n[hooks.state.{json.dumps(hook['key'])}]\ntrusted_hash = {json.dumps(hook['currentHash'])}\n"
        discovery.close()
        (home / "config.toml").write_text(config)
        owner = RPC(args.codex, home, root, "owner")
        clients.append(owner)
        listing = owner.request("hooks/list", {"cwds": [str(root)]})
        evidence["trusted_hooks"] = listing
        subprocess.run([str(installed), "validate-listing"], input=json.dumps(listing), text=True, capture_output=True, check=True)
        pids = root / "native-pids.json"
        provider.command = "exec '" + str(installed).replace("'", "'\\''") + "' run --launch-token " + str(uuid.uuid4()) + " --project " + shlex.quote(str(project)) + " --pid-file " + shlex.quote(str(pids))
        if args.case == "preflight":
            provider.command += " --delay-admission"
        thread = owner.request("thread/start", {"cwd": str(root), "model": "gpt-5.4", "modelProvider": "fixture", "approvalPolicy": "never", "sandbox": "danger-full-access", "ephemeral": False})["thread"]["id"]
        turn = owner.request("turn/start", {"threadId": thread, "input": [{"type": "text", "text": "Run only this hardcoded, harmless lifecycle fixture."}]})["turn"]["id"]
        if args.case == "preflight":
            wait_for((root / "fixture-preflight-ready").exists, 30)
            wait_for(lambda: len(provider.requests) >= 2, 30)
            owner.request("turn/interrupt", {"threadId": thread, "turnId": turn})
            wait_for((root / "fixture-exit.json").exists, 10)
            assert not pids.exists(), "A fixture worker started after cancellation"
        else:
            wait_for(pids.exists, 30)
            identities = json.loads(pids.read_text())
            owned.extend(identities.values())
            native = json.loads(pids.with_suffix(".identity").read_text())
            evidence["native_identity"] = native
            assert native["codex_thread_id"] == thread == native["binding"]["session_id"]
            assert native["parent_pid"] == native["binding"]["owner"]["pid"]
            wait_for(lambda: len(provider.requests) >= 2, 30)
            owner.request("turn/steer", {"threadId": thread, "expectedTurnId": turn, "input": [{"type": "text", "text": "Continue the same harmless fixture."}]})
            time.sleep(idle)
            assert all(alive(pid) for pid in owned), "Healthy work stopped after steering or idle"
            events = [json.loads(line) for line in (root / "hook-events.jsonl").read_text().splitlines()]
            assert not any(event["hook_event_name"] in ["Interrupt", "Stop"] for event in events)
            evidence["healthy_after_steer_seconds"] = idle
            if args.case == "interrupt":
                owner.request("turn/interrupt", {"threadId": thread, "turnId": turn})
            elif args.case == "stop":
                provider.finish.set()
                wait_for(lambda: any(m.get("method") == "turn/completed" and m.get("params", {}).get("turn", {}).get("id") == turn for m in owner.messages))
            elif args.case == "owner-death":
                os.kill(native["binding"]["owner"]["pid"], signal.SIGKILL)
                owner.close()
            elif args.case == "plugin-remove":
                subprocess.run([args.codex, "plugin", "remove", "native-lifecycle-fixture@native-lifecycle-fixture"], cwd=root, env=env, text=True, capture_output=True, check=True)
            wait_for((root / "fixture-exit.json").exists, 10)
            wait_for(lambda: not any(alive(pid) for pid in owned), 10)
        evidence["runtime_exit"] = json.loads((root / "fixture-exit.json").read_text())
        if args.case == "preflight":
            assert "admission_rejected" in evidence["runtime_exit"]["reason"]
        evidence["passed"] = True
        print(json.dumps({key: evidence[key] for key in ["case", "host_version", "runtime_exit", "passed"]}, indent=2), flush=True)
    except Exception as error:
        evidence["error"] = repr(error)
        raise
    finally:
        provider.close()
        for client in clients:
            client.close()
        for pid in owned:
            if alive(pid):
                try:
                    os.kill(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
        hook_log = root / "hook-events.jsonl"
        evidence["hook_events"] = [json.loads(line) for line in hook_log.read_text().splitlines()] if hook_log.exists() else []
        for index, client in enumerate(clients):
            (root / f"rpc-{index}.json").write_text(json.dumps(client.messages, indent=2))
        (root / "mock-requests.json").write_text(json.dumps(provider.requests, indent=2))
        (root / "result.json").write_text(json.dumps(evidence, indent=2))


if __name__ == "__main__":
    main()
