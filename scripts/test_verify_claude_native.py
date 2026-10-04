"""Deterministic native qualification evidence checks; no host or model launches."""

import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

import build
import package_release
import qualify_release
import verify_claude_native as native


class QualificationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="delm-native-evidence-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.project = self.root / "project"
        self.project.mkdir()
        self.original = {}
        for name in ["KEEP.txt", "STAGED.txt", "CLAUDE.md", ".git/index"]:
            path = self.project / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"original staged or untracked bytes\0\xff")
            self.original[name] = native.digest(path)
        for name in package_release.CLAUDE_PROOF_FILES:
            (self.project / name).write_text("// deterministic delivered fixture\n")
        self.run_dir = self.root / "run"
        self.workers = [{"worker": i, "cwd": str(self.run_dir / f"workspace/worker-{i}")}
                        for i in [1, 2]]
        self.tools = ["Agent", "Bash", "Read", "mcp__plugin_delm_delm__delm_status"]
        self.events = [
            {"event": "model_step", "at": 100, "agent": None},
            {"event": "first_step", "at": 100, "agent": None, "model": "claude-opus-4-6",
             "cwd": str(self.project), "tools": self.tools[:3]},
            {"event": "parent_tools", "phase": "model_step", "at": 100, "tools": self.tools[:3]},
            {"event": "parent_tools", "phase": "before_spawn", "at": 110, "tools": self.tools},
        ]
        for i, worker in enumerate(self.workers, 1):
            self.events.extend([
                {"event": "spawn", "at": 110 + i, "agent": f"peer-{i}", "parent": None,
                 "fork": True, "cwd": worker["cwd"], "permissionMode": "auto", "denied": None},
                {"event": "model_step", "at": 120 + i, "agent": f"peer-{i}"},
                {"event": "first_step", "at": 120 + i, "agent": f"peer-{i}", "model": "claude-opus-4-6",
                 "cwd": worker["cwd"], "tools": self.tools},
            ])
        self.journal = [{"kind": "claude_prepared", "data": {"workers": self.workers}}]
        for i, name in [(1, "slug.mjs"), (2, "stats.mjs")]:
            self.journal.append({"kind": "claude_coordination", "data": {
                "worker": i, "tool": "delm_publish", "arguments": {"paths": [name]}, "response": {}}})
        self.journal.append({"kind": "claude_final", "data": {
            "delivery": {"delivered": True, "cleanup_complete": True}}})
        self.plugin = self.root / "plugin"
        for name in [*build.CLAUDE_PACKAGE_FILES, "bin/delm"]:
            path = self.plugin / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("{}" if name.endswith(".json") else "fixture bytes\n")
        self.metadata = {
            "host_version_before": "2.1.289 (Claude Code)", "architecture": "arm64",
            "binary_sha256": native.digest(self.plugin / "bin/delm"),
            "adapter_sha256": build.claude_adapter_digest(self.plugin),
            "fixture_sha256": native.digest(Path(native.__file__)),
            "runtime_sources_sha256": "a" * 64,
        }
        self.source_patch = mock.patch.object(native, "source_state", return_value={"runtimeSourcesSha256": "a" * 64})
        self.source_patch.start()
        self.addCleanup(self.source_patch.stop)

    def inspect(self):
        return native.inspect(self.project, self.original, self.events, self.journal, self.run_dir)

    def final_report(self):
        report = self.inspect()
        report.update({"delivered_output_checked": True, "deadline_stop_requested": False,
                       "native_final_handoff_completed": True,
                       "forced_host_stop": False, "host_version_after": self.metadata["host_version_before"]})
        return report

    def peer(self, number=1):
        return next(row for row in self.events if row["event"] == "first_step" and row.get("agent") == f"peer-{number}")

    def test_realistic_two_fork_evidence_passes_and_matches_release_schema(self):
        report = native.finalize_report(self.final_report(), self.plugin, self.metadata)
        self.assertTrue(report["passed"])
        self.assertEqual([row["parent_at"] for row in report["tool_pool_parent_witnesses"]], [110, 110])
        record = native.qualification_record(self.project, self.events, report, self.metadata)
        self.assertEqual(record["modelCalls"], 3)
        self.assertEqual(record["adapterSha256"], build.claude_adapter_digest(self.plugin))
        metadata = {
            "claudeQualification": {"arm64": record}, "claudeQualifiedArchitectures": ["arm64"],
            "claudeQualificationFixtureSha256": self.metadata["fixture_sha256"],
            "claudeAdapterSha256": self.metadata["adapter_sha256"],
            "runtimeSourcesSha256": self.metadata["runtime_sources_sha256"],
            "files": {"bin/delm": {"sha256": self.metadata["binary_sha256"]}},
        }
        package_release.verify_claude_qualification(metadata)
        metadata["claudeAdapterSha256"] = "b" * 64
        with self.assertRaisesRegex(RuntimeError, "mismatched"):
            package_release.verify_claude_qualification(metadata)

    def test_missing_peer_or_unrelated_peer_identity_fails(self):
        self.events.remove(self.peer(2))
        self.assertFalse(self.inspect()["passed"])
        self.events.append(dict(self.peer(), agent="unrelated", cwd=self.workers[1]["cwd"]))
        report = self.inspect()
        self.assertFalse(report["exactly_two_native_forks"])
        self.assertFalse(report["passed"])

    def test_duplicate_or_denied_forks_fail(self):
        spawns = [row for row in self.events if row["event"] == "spawn"]
        spawns[1]["agent"] = spawns[0]["agent"]
        self.assertFalse(self.inspect()["passed"])
        spawns[1]["agent"] = "peer-2"
        spawns[0]["denied"] = "native denial"
        self.assertFalse(self.inspect()["passed"])

    def test_different_or_missing_models_fail(self):
        self.peer()["model"] = "different-model"
        self.assertFalse(self.inspect()["matching_native_models"])
        self.assertFalse(self.inspect()["passed"])
        for row in self.events:
            row.pop("model", None)
        self.assertFalse(self.inspect()["passed"])

    def test_wrong_or_swapped_peer_workspaces_fail(self):
        self.peer()["cwd"] = str(self.project)
        self.assertFalse(self.inspect()["passed"])
        self.peer()["cwd"], self.peer(2)["cwd"] = self.workers[1]["cwd"], self.workers[0]["cwd"]
        self.assertFalse(self.inspect()["matching_native_worker_cwds"])
        self.assertFalse(self.inspect()["passed"])

    def test_later_pool_discovery_does_not_prove_inheritance(self):
        self.events = [row for row in self.events if row["event"] != "parent_tools"]
        self.events.append({"event": "parent_tools", "phase": "model_step", "at": 300, "tools": self.tools})
        report = self.inspect()
        self.assertFalse(report["matching_native_tool_pools"])
        self.assertFalse(report["passed"])

    def test_stale_matching_pool_cannot_override_newer_mismatch(self):
        self.events.append({"event": "parent_tools", "phase": "before_spawn", "at": 111, "tools": [*self.tools, "required"]})
        report = self.inspect()
        self.assertEqual(report["tool_pool_parent_witnesses"][0]["parent_at"], 111)
        self.assertFalse(report["passed"])

    def test_async_peer_extras_require_a_later_parent_witness_and_record_times(self):
        extra = "mcp__ordinary__late_connected"
        self.peer()["tools"] = [*self.tools, extra]
        self.peer(2)["tools"] = [*self.tools, extra]
        self.assertFalse(self.inspect()["passed"])
        self.events.append({"event": "parent_tools", "phase": "model_step", "at": 300,
                            "tools": [*self.tools, extra]})
        report = self.inspect()
        self.assertTrue(report["passed"])
        for witness in report["tool_pool_parent_witnesses"]:
            self.assertEqual(witness["asynchronously_connected_tools"], [extra])
            self.assertEqual(witness["extra_parent_witness_at"], 300)
            self.assertEqual(witness["missing_parent_tools"], [])

    def test_later_parent_inventory_cannot_excuse_missing_fork_tools(self):
        self.peer()["tools"] = self.tools[:3]
        self.peer(2)["tools"] = self.tools[:3]
        self.events.append({"event": "parent_tools", "phase": "model_step", "at": 300, "tools": self.tools[:3]})
        report = self.inspect()
        self.assertFalse(report["passed"])
        self.assertEqual(report["tool_pool_parent_witnesses"][0]["missing_parent_tools"], self.tools[3:])

    def test_later_changed_pool_does_not_retroactively_fail_valid_inheritance(self):
        self.events.append({"event": "parent_tools", "phase": "model_step", "at": 300, "tools": ["later"]})
        self.assertTrue(self.inspect()["passed"])

    def test_worker_pools_must_match_each_other(self):
        self.peer(2)["tools"] = self.tools[:3]
        self.assertFalse(self.inspect()["passed"])

    def test_cleanup_receipt_and_actual_worker_and_baseline_removal_are_required(self):
        for path in [Path(self.workers[0]["cwd"]), Path(self.workers[1]["cwd"]), self.run_dir / "workspace/baseline"]:
            with self.subTest(path=path.name):
                path.mkdir(parents=True)
                self.assertFalse(self.inspect()["worker_trees_removed"])
                self.assertFalse(self.inspect()["passed"])
                path.rmdir()
        for receipt in [False, None]:
            self.journal[-1]["data"]["delivery"]["cleanup_complete"] = receipt
            self.assertFalse(self.inspect()["passed"])

    def test_original_bytes_and_successful_publications_are_required(self):
        (self.project / "STAGED.txt").write_text("changed staged user bytes")
        self.assertFalse(self.inspect()["passed"])
        (self.project / "STAGED.txt").write_bytes(b"original staged or untracked bytes\0\xff")
        self.journal[2]["data"]["response"] = {"error": "publish rejected"}
        self.assertFalse(self.inspect()["both_workers_published_files"])
        self.assertFalse(self.inspect()["passed"])

    def test_stops_host_changes_or_failed_output_cannot_emit_qualification(self):
        for field, value in [("forced_host_stop", True), ("deadline_stop_requested", True),
                             ("host_version_after", "2.1.290 (Claude Code)"), ("delivered_output_checked", False)]:
            with self.subTest(field=field):
                report = self.final_report()
                report[field] = value
                native.finalize_report(report, self.plugin, self.metadata)
                self.assertFalse(report["passed"])
                self.assertIsNone(native.qualification_record(self.project, self.events, report, self.metadata))

    def test_changed_adapter_runtime_source_or_helper_fails_provenance(self):
        for field in ["binary_sha256", "adapter_sha256", "runtime_sources_sha256", "fixture_sha256"]:
            with self.subTest(field=field):
                metadata = dict(self.metadata, **{field: "0" * 64})
                report = native.finalize_report(self.final_report(), self.plugin, metadata)
                self.assertFalse(report["sources_and_runtime_unchanged"])
                self.assertFalse(report["passed"])
        (self.plugin / "hooks/delm.js").write_text("changed executing adapter\n")
        report = native.finalize_report(self.final_report(), self.plugin, self.metadata)
        self.assertFalse(report["passed"])
        (self.plugin / ".claude-plugin/plugin.json").unlink()
        report = native.finalize_report(self.final_report(), self.plugin, self.metadata)
        self.assertFalse(report["passed"])

    def test_rosetta_is_rejected_before_project_or_host_setup(self):
        arguments = ["verify_claude_native.py", "--plugin", str(self.plugin), "--out", str(self.root / "new"), "--authorize-model-use"]
        with mock.patch("sys.argv", arguments), mock.patch.object(qualify_release.sys, "platform", "darwin"), \
             mock.patch.object(qualify_release.platform, "machine", return_value="x86_64"), \
             mock.patch.object(qualify_release.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "1\n", "")), \
             mock.patch.object(native, "prepare") as prepare, mock.patch.object(native.subprocess, "Popen") as launch:
            with self.assertRaisesRegex(RuntimeError, "not emulation"):
                native.main()
            prepare.assert_not_called()
            launch.assert_not_called()
        self.assertFalse((self.root / "new").exists())

    def test_bounded_delivered_check_failures_leave_evidence(self):
        for error in [subprocess.TimeoutExpired("node", 10), FileNotFoundError("node unavailable")]:
            with self.subTest(error=type(error).__name__), mock.patch.object(native.subprocess, "run", side_effect=error):
                self.assertFalse(native.check_delivery(self.project, self.root))
                evidence = json.loads((self.root / "delivered-check.json").read_text())
                self.assertFalse(evidence["passed"])
                self.assertTrue(evidence["error"])

    def test_final_handoff_waits_for_its_parent_turn_and_is_bounded(self):
        events = [{"event": "handoff_started", "turn": "final-turn"}]
        self.assertFalse(native.handoff_wait_finished(events, 100, 124.99))
        self.assertTrue(native.handoff_wait_finished(events, 100, 125))
        for event in [
            {"event": "turn_complete", "turn": "earlier-parent", "reason": "answer"},
            {"event": "turn_complete", "turn": "final-turn", "agent": "peer-1", "reason": "answer"},
            {"event": "turn_complete", "turn": "final-turn", "reason": "answer", "aborted": True},
        ]:
            self.assertFalse(native.handoff_finished([*events, event]))
        events.append({"event": "turn_complete", "turn": "final-turn", "reason": "answer", "aborted": False})
        self.assertTrue(native.handoff_wait_finished(events, 100, 101))

    def test_timed_out_final_handoff_cannot_qualify_successful_delivery(self):
        report = self.final_report()
        report["native_final_handoff_completed"] = False
        self.assertFalse(native.finalize_report(report, self.plugin, self.metadata)["passed"])


if __name__ == "__main__":
    unittest.main()
