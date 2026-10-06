"""No-model regression for native selector confirmation and launch environment.

Build `cargo build --example native-lifecycle-fixture` before running this file.
The MCP process deliberately receives a smaller environment than the trusted
command hook. No real host, account, model, or installed plugin is used.
"""
import json
import os
from pathlib import Path
import selectors
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

from verify_fresh_install import identity_running


SOURCE = Path(__file__).resolve().parent.parent
FIXTURE = Path(os.environ.get("DELM_LIFECYCLE_FIXTURE", SOURCE / "target/debug/examples/native-lifecycle-fixture"))


def send(process, message):
    process.stdin.write(json.dumps(message) + "\n")
    process.stdin.flush()


def receive(process, timeout=5):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        if not selector.select(timeout):
            raise TimeoutError("Native selector fixture did not reply")
        line = process.stdout.readline()
    if not line:
        raise RuntimeError("Native selector fixture exited without a reply")
    return json.loads(line)


@unittest.skipUnless(sys.platform == "darwin" and FIXTURE.is_file(), "Build the native-lifecycle-fixture example on macOS first")
class NativeSelectionEnvironmentTests(unittest.TestCase):
    def test_confirmed_launch_inherits_command_environment_and_occurs_once(self):
        for count in (3, 4):
            with self.subTest(agents=count), tempfile.TemporaryDirectory(prefix="delm-selector-env-") as temporary:
                root = Path(temporary).resolve()
                project, package = root / "project", root / "plugin"
                home, codex_home, mcp_home = root / "home", root / "native-config", root / "mcp-home"
                for path in (project, package / "bin", package / "hooks", home, codex_home, mcp_home):
                    path.mkdir(parents=True)
                executable = package / "bin/delm"
                shutil.copy2(FIXTURE, executable)
                shutil.copy2(SOURCE / "hooks/hooks.json", package / "hooks/hooks.json")
                shutil.copy2(SOURCE / ".mcp.json", package / ".mcp.json")
                session, turn = str(uuid.uuid4()), str(uuid.uuid4())
                hook_input = {"hook_event_name": "UserPromptSubmit", "session_id": session,
                              "turn_id": turn, "prompt": "$delm:run Harmless environment fixture.",
                              "cwd": str(project)}
                native_path = str(root / "native-only-bin") + ":/usr/bin:/bin"
                native_env = {"HOME": str(home), "CODEX_HOME": str(codex_home), "PATH": native_path,
                              "DELM_TEST_NATIVE_ENV": "native-command-only", "CODEX_THREAD_ID": session}
                mcp_env = {"HOME": str(mcp_home), "PATH": "/usr/bin:/bin"}
                processes, child_identity = [], None
                try:
                    hook = subprocess.Popen([str(executable), "capture-lifecycle-hook"],
                                            cwd=project, env=native_env, stdin=subprocess.PIPE,
                                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                    processes.append(hook)
                    hook.stdin.write(json.dumps(hook_input))
                    hook.stdin.close()
                    hook.stdin = None
                    mcp = subprocess.Popen([str(executable), "selector-mcp"], cwd=project, env=mcp_env,
                                           stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                           stderr=subprocess.PIPE, text=True, bufsize=1)
                    processes.append(mcp)
                    send(mcp, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                        "protocolVersion": "2025-03-26", "capabilities": {"elicitation": {"form": {}}},
                        "clientInfo": {"name": "delm-environment-fixture", "version": "1"}}})
                    self.assertEqual(receive(mcp)["id"], 1)
                    send(mcp, {"jsonrpc": "2.0", "method": "notifications/initialized"})
                    send(mcp, {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                        "name": "select_agents", "arguments": hook_input, "_meta": {"threadId": session}}})
                    form = receive(mcp)
                    self.assertEqual(form.get("method"), "elicitation/create", form)
                    self.assertEqual(form["params"]["requestedSchema"]["properties"]["agents"]["default"], "2")
                    self.assertFalse((project / "capture-starts.jsonl").exists(), "Child launched before confirmation")
                    self.assertIsNone(hook.poll(), "Native command stopped waiting before confirmation")
                    send(mcp, {"jsonrpc": "2.0", "id": form["id"], "result": {
                        "action": "accept", "content": {"agents": str(count)}}})
                    reply = receive(mcp)
                    self.assertEqual(reply["id"], 2)
                    self.assertFalse(reply.get("isError", False), reply)
                    stdout, stderr = hook.communicate(timeout=5)
                    self.assertEqual(hook.returncode, 0, stderr)
                    self.assertIn("follow", stdout)
                    evidence_file = project / "captured-environment.json"
                    until = time.monotonic() + 5
                    while True:
                        try:
                            evidence = json.loads(evidence_file.read_text())
                            break
                        except (FileNotFoundError, json.JSONDecodeError):
                            if time.monotonic() >= until:
                                raise TimeoutError("Confirmed child did not record its fixture environment")
                            time.sleep(0.01)
                    child_identity = evidence["process_identity"]
                    self.assertEqual(evidence["worker_count"], count)
                    self.assertEqual(evidence["codex_home"], str(codex_home))
                    self.assertEqual(evidence["path"], native_path)
                    self.assertEqual(evidence["marker"], "native-command-only")
                    self.assertEqual(evidence["thread"], session)
                    repeated = subprocess.run([str(executable), "capture-lifecycle-hook"], cwd=project,
                                              env=native_env, input=json.dumps(hook_input), text=True,
                                              capture_output=True, timeout=5)
                    self.assertEqual(repeated.returncode, 0, repeated.stderr)
                    self.assertEqual(len((project / "capture-starts.jsonl").read_text().splitlines()), 1)
                finally:
                    (project / "release-captured-child").touch()
                    finished = True
                    if (project / "captured-environment.json").exists():
                        until = time.monotonic() + 3
                        while not (project / "captured-child-finished").exists() and time.monotonic() < until:
                            time.sleep(0.01)
                        finished = (project / "captured-child-finished").exists()
                        if not finished and child_identity is not None and identity_running(child_identity):
                            os.kill(child_identity["pid"], signal.SIGKILL)
                    for process in processes:
                        if process.poll() is None:
                            process.terminate()
                        try:
                            process.wait(timeout=3)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=3)
                        for stream in (process.stdin, process.stdout, process.stderr):
                            if stream:
                                stream.close()
                    registry = Path("/tmp").resolve() / f"delm-{os.getuid()}/lifecycle"
                    for path in registry.glob(f"*{session}*"):
                        path.unlink(missing_ok=True)
                    self.assertTrue(finished, "Owned fixture child did not finish")


if __name__ == "__main__":
    unittest.main()
