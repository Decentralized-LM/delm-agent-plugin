#!/usr/bin/env python3
"""No-model checks for the optional fresh-install qualification helper."""
import json
from pathlib import Path
import sqlite3
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from verify_fresh_install import inspect_run, read_task, remove_auth_reference, snapshot, task_handoff


class QualificationTests(unittest.TestCase):
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
