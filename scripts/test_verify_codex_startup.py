"""Focused helper checks; no hosts, accounts or model processes are launched."""
import errno
import copy
import json
from pathlib import Path
import signal
import tempfile
import unittest
from unittest.mock import Mock, patch

import verify_codex_startup as startup


class StartupHelpersTest(unittest.TestCase):
    def test_native_title_request_is_not_counted_as_a_parent_or_worker_turn(self):
        payload = {"text": {"format": {"schema": {"properties": {"title": {}}}}},
                   "instructions": "You are worker 1."}
        self.assertEqual(startup.classify_request(payload), ("title", 0))

    def test_worker_classification_requires_the_real_two_peer_instruction(self):
        self.assertEqual(startup.classify_request({"instructions": "You are worker 1."}), ("worker", 1))
        self.assertEqual(startup.classify_request({"instructions": "You are worker 2."}), ("worker", 2))
        self.assertEqual(startup.classify_request({"input": "Build two features"}), ("parent", 0))
        with self.assertRaises(RuntimeError):
            startup.classify_request({"instructions": "You are worker 1. You are worker 2."})

    def test_tools_are_resolved_from_native_inventory_with_no_invented_name(self):
        payload = {"tools": [{"type": "function", "name": "mcp__delm_coordination_1__delm_publish"}]}
        self.assertEqual(startup.tool_name(payload, "delm_publish"), "mcp__delm_coordination_1__delm_publish")
        with self.assertRaises(RuntimeError):
            startup.tool_name(payload, "delm_complete")
        payload["tools"].append({"name": "mcp__other__delm_publish"})
        with self.assertRaises(RuntimeError):
            startup.tool_name(payload, "delm_publish")

    def test_responses_lite_inventory_and_native_code_executor_are_supported(self):
        payload = {"input": [{"type": "additional_tools", "tools": [{"type": "namespace", "name": "functions",
            "tools": [{"name": "exec", "description": "declare const tools: { exec_command(args: {}): Promise<unknown>; };"}]}]}]}
        result = startup.native_call(payload, "exec_command", {"cmd": "true"}, "owned-command")
        self.assertEqual(result["type"], "custom_tool_call")
        self.assertEqual(result["name"], "exec")
        self.assertEqual(result["input"], 'text(await tools.exec_command({"cmd": "true"}));')
        with self.assertRaises(RuntimeError):
            startup.native_call(payload, "unlisted_tool", {}, "unknown")

    def test_coordination_discovery_is_bound_to_exact_worker_server(self):
        payload = {"instructions": "You are worker 2.", "tools": [{"name": "exec"}]}
        result = startup.native_call(payload, "delm_publish", {"paths": []}, "publish")
        self.assertIn('t.name==="mcp__delm_coordination_2__delm_publish"', result["input"])
        self.assertIn("found.length!==1", result["input"])

    def test_fixture_user_approves_only_current_worker_native_coordination(self):
        saved = {"workers": [{"thread": "thread-1", "turn": "turn-1"}, {"thread": "thread-2", "turn": "turn-2"}]}
        details = {"worker": 1, "method": "mcpServer/elicitation/request", "request": {
            "serverName": "delm_coordination_1", "threadId": "thread-1", "turnId": "turn-1", "mode": "form",
            "requestedSchema": {"type": "object", "properties": {}}, "_meta": {"codex_approval_kind": "mcp_tool_call"},
            "message": 'Allow the delm_coordination_1 MCP server to run tool "delm_publish"?'}}
        self.assertTrue(startup.fixture_approval(details, saved))
        for key, value in [("serverName", "remote"), ("threadId", "other"), ("turnId", "retired"),
                           ("requestedSchema", {"type": "object", "properties": {"password": {}}}),
                           ("message", 'Allow the delm_coordination_1 MCP server to run tool "delete_files"?')]:
            changed = copy.deepcopy(details)
            changed["request"][key] = value
            self.assertFalse(startup.fixture_approval(changed, saved), key)

    def test_publication_evidence_requires_successful_native_results_for_both_workers(self):
        def publication(worker):
            return {"kind": "native", "data": {"method": "item/completed", "params": {"item": {
                "type": "mcpToolCall", "tool": "delm_publish", "server": f"delm_coordination_{worker}",
                "status": "completed", "error": None, "arguments": {"paths": ["hello.txt"] if worker == 1 else []},
                "result": {"content": [{"text": json.dumps({"result": {"publication_id": worker}})}]}}}}}
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            path = run / "events.jsonl"
            first, second = publication(1), publication(2)
            path.write_text(json.dumps(first) + "\n")
            self.assertFalse(startup.publication_evidence(run))
            path.write_text(json.dumps(first) + "\n" + json.dumps(second) + "\n")
            self.assertTrue(startup.publication_evidence(run))
            second["data"]["params"]["item"]["error"] = {"message": "Rejected"}
            path.write_text(json.dumps(first) + "\n" + json.dumps(second) + "\n")
            self.assertFalse(startup.publication_evidence(run))
            path.write_text(json.dumps({"kind": "native", "data": {"instructions": "delm_publish delm_complete"}}) + "\n")
            self.assertFalse(startup.publication_evidence(run))

    def test_ordinary_reply_cannot_be_satisfied_by_the_echoed_prompt(self):
        self.assertNotIn(startup.ORDINARY_REPLY, startup.ORDINARY_PROMPT)

    def test_group_already_exited_during_permission_error_is_not_cleanup_failure(self):
        process = Mock(pid=123)
        process.poll.side_effect = [None, None, 0]
        with patch.object(startup.os, "getpgid", return_value=123), patch.object(
                startup.os, "killpg", side_effect=PermissionError(errno.EPERM, "exited group")):
            startup.stop_process_group(process, lambda: .1)
        process.send_signal.assert_not_called()

    def test_changed_group_uses_owned_child_handle(self):
        process = Mock(pid=123)
        process.poll.return_value = None
        with patch.object(startup.os, "getpgid", return_value=456), patch.object(startup.os, "killpg") as kill:
            startup.stop_process_group(process, lambda: .1)
        kill.assert_not_called()
        process.send_signal.assert_called_once_with(signal.SIGTERM)


if __name__ == "__main__":
    unittest.main()
