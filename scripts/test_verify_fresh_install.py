#!/usr/bin/env python3
"""No-model checks for the optional fresh-install qualification helper."""
import json
from pathlib import Path
import signal
import sqlite3
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, call, patch

from verify_codex_selector import stop_process_group

from verify_fresh_install import (EvidenceRPC, git_state, inspect_run, qualification_hooks,
                                 read_task, remove_auth_reference, selector_response, snapshot, task_handoff)


class SelectorCleanupTests(unittest.TestCase):
    def test_exiting_launcher_is_reaped_after_kill_permission_race(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired("codex", .5), 0]
        with patch("verify_codex_selector.os.killpg", side_effect=[None, PermissionError("exiting")]) as kill:
            stop_process_group(process, lambda limit: limit)
        self.assertEqual(kill.call_args_list, [call(123, signal.SIGTERM), call(123, signal.SIGKILL)])
        self.assertEqual(process.wait.call_count, 2)

    def test_permission_denied_for_a_surviving_launcher_is_not_hidden(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        process.wait.side_effect = subprocess.TimeoutExpired("codex", .5)
        with patch("verify_codex_selector.os.killpg", side_effect=PermissionError("still alive")):
            with self.assertRaisesRegex(PermissionError, "still alive"):
                stop_process_group(process, lambda limit: limit)

    def test_launcher_that_survives_both_signals_still_fails(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        process.wait.side_effect = subprocess.TimeoutExpired("codex", .5)
        with patch("verify_codex_selector.os.killpg"):
            with self.assertRaises(subprocess.TimeoutExpired):
                stop_process_group(process, lambda limit: limit)


class QualificationTests(unittest.TestCase):
    def selector(self):
        return {"id": "native-form-1", "method": "mcpServer/elicitation/request", "params": {
            "threadId": "qualification-thread", "turnId": "qualification-turn",
            "serverName": "delm_selector", "mode": "form", "message": "How many agents?",
            "requestedSchema": {"type": "object", "required": ["agents"], "properties": {
                "agents": {"type": "string", "enum": ["2", "3", "4"], "default": "2",
                           "enumNames": ["2 agents (default)", "3 agents", "4 agents"]}}}}}

    def answer(self, message, count=2):
        return selector_response(message, count, "qualification-thread", "qualification-turn")

    def test_exact_native_form_replays_only_the_explicit_qualification_choice(self):
        for count in (2, 3, 4):
            request = self.selector()
            before = json.dumps(request)
            self.assertEqual(self.answer(request, count), {
                "id": "native-form-1", "result": {"action": "accept", "content": {"agents": str(count)}}})
            self.assertEqual(json.dumps(request), before)
        # App-server's protocol explicitly permits an uncorrelated null turn ID.
        request["params"]["turnId"] = None
        self.assertEqual(self.answer(request)["result"]["content"], {"agents": "2"})

    def test_selector_rejects_other_threads_tools_forms_and_changed_choices(self):
        for field, value in (("threadId", "another-thread"), ("turnId", "another-turn"),
                             ("serverName", "other_server"), ("mode", "url"), ("message", "Different question")):
            request = self.selector()
            request["params"][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.answer(request)
        for field, value in (("enum", ["2", "3", "4", "5"]), ("default", "4"), ("type", "integer"),
                             ("enumNames", ["2", "3", "4"])):
            request = self.selector()
            request["params"]["requestedSchema"]["properties"]["agents"][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.answer(request)
        for count in (1, 5, True, "4", None):
            with self.subTest(count=count), self.assertRaises(ValueError):
                self.answer(self.selector(), count)
        for changed in ({"id": None}, {"id": True}, {"method": "item/tool/requestUserInput"}, {"params": None}):
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                self.answer({**self.selector(), **changed})
        for schema in (None, {}, {"type": "object", "required": ["agents"], "properties": None}):
            request = self.selector()
            request["params"]["requestedSchema"] = schema
            with self.subTest(schema=schema), self.assertRaises(ValueError):
                self.answer(request)

    def test_qualification_requires_all_five_trusted_native_handlers(self):
        hooks = [{"eventName": event, "handlerType": kind, "enabled": True, "trustStatus": "trusted",
                  "timeoutSec": 330 if event == "userPromptSubmit" else 3 if event == "interrupt" else 5}
                 for event, kind in [("preToolUse", "command"), ("userPromptSubmit", "command"),
                                     ("userPromptSubmit", "mcpTool"), ("interrupt", "command"), ("stop", "command")]]
        hooks[2].update(server="delm_selector", tool="select_agents")
        listing = {"data": [{"hooks": hooks}]}
        self.assertEqual(qualification_hooks(listing, "trusted"), hooks)
        for wrong in (hooks[:4], hooks + [hooks[0]], [dict(hook, enabled=False) for hook in hooks]):
            with self.assertRaises(RuntimeError):
                qualification_hooks({"data": [{"hooks": wrong}]}, "trusted")
        for index, hook in enumerate(hooks):
            changed = [dict(value) for value in hooks]
            changed[index]["timeoutSec"] = 5 if hook["timeoutSec"] == 330 else 330
            with self.subTest(event=hook["eventName"], kind=hook["handlerType"]), self.assertRaises(RuntimeError):
                qualification_hooks({"data": [{"hooks": changed}]}, "trusted")
        hooks[2]["server"] = "another_server"
        with self.assertRaises(RuntimeError):
            qualification_hooks(listing, "trusted")

    def test_initialize_advertises_documented_form_capability(self):
        client = EvidenceRPC.__new__(EvidenceRPC)
        client.outgoing = []
        request = {"id": 1, "method": "initialize", "params": {"capabilities": {"experimentalApi": True}}}
        with patch("verify_native_lifecycle.RPC.send") as send:
            client.send(request)
        self.assertEqual(send.call_args.args[0]["params"]["capabilities"]["extensions"], {"openai/form": {}})
        self.assertNotIn("extensions", request["params"]["capabilities"])

    def test_source_delivery_may_change_code_but_not_git_state(self):
        before = {".git/index": {"sha256": "original"}, "src/app.py": {"sha256": "old"}}
        after = {".git/index": {"sha256": "original"}, "src/app.py": {"sha256": "new"}}
        self.assertEqual(git_state(before), git_state(after))
        after[".git/index"] = {"sha256": "modified"}
        self.assertNotEqual(git_state(before), git_state(after))

    def test_inspection_reports_original_delivery_and_removed_workspaces(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            run = root / "run"
            (run / "workspace/delivery").mkdir(parents=True)
            original = root / "project"
            original.mkdir()
            (run / "run.json").write_text(json.dumps({"status": "delivered", "request": {
                "task": "Fixture", "model": "fixture", "reasoning_effort": "high"},
                "workspace": {"workers": [str(run / "workspace/worker-1"), str(run / "workspace/worker-2")],
                              "baseline": str(run / "workspace/baseline")}, "workers": []}))
            (run / "workspace/delivery/result.json").write_text(json.dumps({
                "delivered": True, "project": str(original), "verification_required": True}))
            evidence = inspect_run(run, root)
            self.assertTrue(evidence["temporary_workspaces_removed"])
            self.assertTrue(evidence["delivery"]["verification_required"])
            self.assertEqual(evidence["delivery"]["project"], str(original))
    def test_task_file_preserves_crlf_and_snapshot_records_empty_directories(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            task = root / "request.txt"
            task.write_bytes(b"$delm:run Build it.\r\n")
            self.assertEqual(read_task(task), "$delm:run Build it.\r\n")
            self.assertFalse(task_handoff(read_task(task), "$delm:run Build it.\n")["task_content_unchanged"])
            before = snapshot(root)
            (root / "empty").mkdir()
            self.assertNotEqual(snapshot(root), before)

    def test_only_one_terminal_newline_may_differ(self):
        self.assertTrue(task_handoff("Build it.\n", "Build it.")["task_content_unchanged"])
        self.assertFalse(task_handoff("Build it.\n", "Build it.")["task_exact_bytes"])
        self.assertTrue(task_handoff("Build it.", "Build it.")["task_exact_bytes"])
        for changed in ["Build  it.", " Build it.", "Build it. ", "Build it.\n\n", "Build it.\r\n"]:
            self.assertFalse(task_handoff("Build it.", changed)["task_content_unchanged"])

    def test_auth_reference_remains_until_native_processes_stop(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home, runs = root / "home", root / "runs"
            home.mkdir()
            original = root / "original-auth"
            original.write_bytes(b"opaque fixture: never read by cleanup")
            reference = home / "auth.json"
            reference.symlink_to(original)
            launch = runs / "run/launches/launch"
            launch.mkdir(parents=True)
            (launch / "watchdog.json").write_text(json.dumps({"host": {"pid": 101}, "runtime": {"pid": 102}}))
            client = SimpleNamespace(process=SimpleNamespace(poll=lambda: 0))
            with patch("verify_fresh_install.identity_running", return_value=True):
                self.assertFalse(remove_auth_reference(home, runs, [client], timeout=0))
                self.assertTrue(reference.is_symlink())
            with patch("verify_fresh_install.identity_running", return_value=False):
                self.assertTrue(remove_auth_reference(home, runs, [client], timeout=0))
            self.assertFalse(reference.is_symlink())
            self.assertTrue(original.is_file())

    def test_board_evidence_keeps_wal_events_without_mutating_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            run = root / "run"
            (run / "board").mkdir(parents=True)
            (run / "run.json").write_text(json.dumps({"status": "complete", "request": {
                "task": "Fixture", "model": "fixture", "reasoning_effort": "high"},
                "workspace": {"workers": []}, "workers": []}))
            original = run / "board/board.sqlite3"
            connection = sqlite3.connect(original)
            try:
                connection.execute("PRAGMA journal_mode=WAL")
                connection.execute("CREATE TABLE events (seq INTEGER,worker INTEGER,kind TEXT,body TEXT)")
                connection.execute("INSERT INTO events VALUES (1,1,'apply','{}')")
                connection.commit()
                before = {path.name: path.read_bytes() for path in original.parent.iterdir()}
                evidence = inspect_run(run, root)
                self.assertEqual(evidence["peer_import_count"], 1)
                self.assertEqual(evidence["board_events"][0]["sequence"], 1)
                self.assertEqual(before, {path.name: path.read_bytes() for path in original.parent.iterdir()})
            finally:
                connection.close()


if __name__ == "__main__":
    unittest.main()
