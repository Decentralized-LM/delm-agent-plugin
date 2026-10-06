"""Source maintenance protection against active work and incomplete recovery."""

import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import build
import maintenance


class MaintenanceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="delm maintenance test ")
        self.addCleanup(temporary.cleanup)
        self.home = Path(temporary.name)
        self.run = self.home / "Library/Application Support/DeLM/runs/11111111-2222-4333-8444-555555555555"
        self.run.mkdir(parents=True)

    def state(self, host="claude", **fields):
        path = self.run / ("claude.json" if host == "claude" else "run.json")
        path.write_text(json.dumps(fields))
        return path

    def check(self, host="claude"):
        maintenance.assert_maintenance_safe(host, self.home)

    def test_absent_storage_does_not_get_created(self):
        missing = self.home / "absent"
        maintenance.assert_maintenance_safe("codex", missing)
        self.assertFalse(missing.exists())

    def test_active_and_uncertain_storage_blocks_without_modification(self):
        for status in ("running", "awaiting_shutdown", "recovery_required"):
            with self.subTest(status=status):
                record = self.state(status=status, finished=False, task="PRIVATE")
                before = record.read_bytes()
                with self.assertRaisesRegex(RuntimeError, "active or needs recovery") as error:
                    self.check()
                self.assertNotIn("PRIVATE", str(error.exception))
                self.assertEqual(before, record.read_bytes())
                self.check("codex")

    def test_linked_record_and_conflicting_hosts_block(self):
        record = self.run / "claude.json"
        external = self.home / "record"
        external.write_text('{"status":"complete","finished":true}')
        record.symlink_to(external)
        with self.assertRaisesRegex(RuntimeError, "safely inspect"):
            self.check()
        record.unlink()
        self.state(status="complete", finished=True)
        self.state("codex", status="running")
        with self.assertRaisesRegex(RuntimeError, "safely inspect"):
            self.check()

    def test_terminal_state_requires_no_workers(self):
        self.state(status="complete", finished=True)
        self.check()
        for status in ("pending", "unconfirmed"):
            self.state(status="complete", finished=True, finalization={"shutdown_ack": status})
            with self.assertRaises(RuntimeError):
                self.check()
        self.state(status="complete", finished=True, finalization={"shutdown_ack": "confirmed"})
        self.check()
        (self.run / "workspace/worker-2").mkdir(parents=True)
        with self.assertRaises(RuntimeError):
            self.check()

    def test_recovery_requires_actual_bytes_and_shutdown(self):
        self.state(status="recovery_required", finished=True)
        recovery = self.run / "workspace/recovery"
        recovery.mkdir(parents=True)
        with self.assertRaises(RuntimeError):
            self.check()
        data = b"preserved output"
        sha256 = hashlib.sha256(data).hexdigest()
        entry = {"kind": "file", "size": len(data), "sha256": sha256, "mode": 0o644,
                 "link_target": None, "xattrs_sha256": "", "xattrs_bytes": 0, "acl_sha256": "", "flags": 0}
        bundle = {"version": 1, "original": str(self.home / "project"), "workers": [{"worker": 0, "changes": {"renders/result": [None, entry]}}]}
        (recovery / "complete.json").write_text(json.dumps(bundle))
        (recovery / sha256).write_bytes(data)
        self.check()
        for malformed in (
            {**bundle, "original": None},
            {**bundle, "workers": [bundle["workers"][0], bundle["workers"][0]]},
            {**bundle, "workers": [{"worker": 0, "changes": {"file": [None, None]}}]},
            {**bundle, "workers": [{"worker": 0, "changes": {"file": [None, {**entry, "mode": None}]}}]},
            {**bundle, "workers": [{"worker": 0, "changes": {"file": [None, {**entry, "kind": "symlink"}]}}]},
            {**bundle, "workers": [{"worker": 0, "changes": {".git/config": [None, entry]}}]},
        ):
            (recovery / "complete.json").write_text(json.dumps(malformed))
            with self.assertRaises(RuntimeError):
                self.check()
        (recovery / "complete.json").write_text(json.dumps(bundle))
        self.state(status="recovery_required", finished=False)
        with self.assertRaises(RuntimeError):
            self.check()
        self.state(status="recovery_required", finished=True)
        (recovery / sha256).write_bytes(b"damaged")
        with self.assertRaises(RuntimeError):
            self.check()

    def test_activation_guard_precedes_replacing_any_package(self):
        source = self.home / "source"
        published = source / ".build/plugin-claude"
        published.mkdir(parents=True)
        (published / "existing").write_text("preserve installed adapter")
        runtime = self.home / "runtime"
        runtime.write_text("fixture")
        with mock.patch.object(build, "stage_package"), mock.patch.object(build.subprocess, "run"), mock.patch.object(build, "validate_claude_runtime"), mock.patch.object(build, "validate_claude_package"), mock.patch.object(build, "assert_maintenance_safe", side_effect=RuntimeError("active run")) as guard:
            with self.assertRaisesRegex(RuntimeError, "active run"):
                build.build(source, runtime, host="claude")
        guard.assert_called_once_with("claude", package=published)
        self.assertEqual((published / "existing").read_text(), "preserve installed adapter")

    def test_recovery_worker_id_must_belong_to_declared_roster(self):
        self.state("codex", status="recovery_required")
        (self.run / "shutdown-report.json").write_text(json.dumps({
            "ownership_resolved": True, "survivors": [], "errors": []}))
        recovery = self.run / "workspace/recovery"
        recovery.mkdir(parents=True)
        bundle = {"version": 1, "original": str(self.home / "project"),
                  "worker_count": 4, "workers": [{"worker": i, "changes": {}} for i in range(4)]}
        manifest = recovery / "complete.json"
        manifest.write_text(json.dumps(bundle))
        self.check("codex")
        for invalid in (None, True, 1, 2, 3, 5, "4"):
            with self.subTest(worker_count=invalid):
                manifest.write_text(json.dumps({**bundle, "worker_count": invalid}))
                with self.assertRaises(RuntimeError):
                    self.check("codex")
        legacy = {key: value for key, value in bundle.items() if key != "worker_count"}
        legacy["workers"] = legacy["workers"][:2]
        manifest.write_text(json.dumps(legacy))
        self.check("codex")
        legacy["workers"][1]["worker"] = 3
        manifest.write_text(json.dumps(legacy))
        with self.assertRaises(RuntimeError):
            self.check("codex")

    def test_unrelated_known_adapter_does_not_block_offline_build(self):
        package = self.home / "active-plugin"
        self.state(status="running", finished=False, package_root=str(package))
        maintenance.assert_maintenance_safe("claude", self.home, package=self.home / "offline-plugin")
        with self.assertRaises(RuntimeError):
            maintenance.assert_maintenance_safe("claude", self.home, package=package)


if __name__ == "__main__":
    unittest.main()
