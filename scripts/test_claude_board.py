"""Model-free integration checks for the installed passive board observer.

Only the runtime subprocess receives the disposable fixture user's environment.
No host process, account, model, controller, or project workspace is launched.
The BoardFixture helpers are also usable by the native UI preview.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import selectors
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
import uuid


SOURCE = Path(__file__).resolve().parent.parent
RUNTIME = Path(os.environ.get("DELM_BOARD_RUNTIME", SOURCE / "target/debug/delm"))


def private_directory(path):
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.chmod(0o700)


def private_json(path, value):
    temporary = path.with_suffix(".view-fixture.tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as stream:
        json.dump(value, stream)
        stream.write("\n")
    temporary.replace(path)


class BoardFixture:
    """Deterministic saved runtime facts, without creating an actual DeLM run."""

    def __init__(self, fixture_home, session_id=None):
        self.fixture_home = Path(fixture_home).resolve()
        self.run_id = str(uuid.uuid4())
        self.session_id = session_id or "board-preview-" + str(uuid.uuid4())
        storage = self.fixture_home / "Library/Application Support/DeLM"
        self.run_dir = storage / "runs" / self.run_id
        for directory in (storage, storage / "runs", self.run_dir, self.run_dir / "board"):
            private_directory(directory)
        self.database = self.run_dir / "board/board.sqlite3"
        self.database.touch(mode=0o600)
        self.connection = sqlite3.connect(self.database, timeout=0.25)
        self.connection.executescript("""
            PRAGMA journal_mode=WAL;
            CREATE TABLE context(id INTEGER PRIMARY KEY, revision INTEGER NOT NULL);
            INSERT INTO context VALUES(1,1);
            CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT, worker INTEGER,
                                revision INTEGER, kind TEXT, body TEXT);
            CREATE TABLE tasks(id INTEGER PRIMARY KEY, author INTEGER, owner INTEGER,
                               state TEXT, body TEXT, updated INTEGER);
            CREATE TABLE workers(worker INTEGER PRIMARY KEY, state TEXT, summary TEXT,
                                 dependency TEXT, updated INTEGER);
            INSERT INTO workers VALUES(1,'working','',NULL,0),(2,'working','',NULL,0);
            CREATE TABLE publications(id INTEGER PRIMARY KEY, worker INTEGER,
                                      revision INTEGER, body TEXT);
            CREATE TABLE check_receipts(id INTEGER PRIMARY KEY, worker INTEGER,
                                        revision INTEGER, body TEXT);
        """)
        self.state = {
            "version": 1, "host": "claude", "session_id": self.session_id,
            "revision": 1, "sequence": 1, "status": "running", "finished": False,
            "task": "PRIVATE PROMPT MUST NOT APPEAR", "token_digest": "PRIVATE AUTHORITY",
            "workers": [
                {"agent_id": "native-agent-1", "turn_id": "turn-1", "revision": 1},
                {"agent_id": "native-agent-2", "turn_id": "turn-2", "revision": 1},
            ],
        }
        private_json(self.run_dir / "claude.json", self.state)
        self.append_journal("native", {"command": "PRIVATE COMMAND MUST NOT APPEAR"})

    def environment(self):
        # This is an isolated child's real home, not an alias used by the test
        # runner. The runner's environment and the actual user's state stay intact.
        environment = dict(os.environ)
        environment["HOME"] = str(self.fixture_home)
        return environment

    def command(self, *extra, runtime=RUNTIME):
        return [str(runtime), "claude", "view", "--run-id", self.run_id,
                "--session-id", self.session_id, *map(str, extra)]

    def read(self, *extra, runtime=RUNTIME, check=True):
        process = subprocess.run(self.command(*extra, runtime=runtime),
                                 env=self.environment(), capture_output=True,
                                 text=True, timeout=5)
        if check and process.returncode:
            raise AssertionError(process.stderr)
        return json.loads(process.stdout) if check else process

    def event(self, worker, kind, body):
        row = self.connection.execute(
            "INSERT INTO events(worker,revision,kind,body) VALUES(?,1,?,?)",
            (worker, kind, json.dumps(body)))
        return row.lastrowid

    def task(self, title, *, owner=None, state="available", description=""):
        body = {"title": title, "description": description,
                "dependencies": [], "kind": "implementation"}
        with self.connection:
            identity = self.event(owner or 1, "task_create", body)
            self.connection.execute("INSERT INTO tasks VALUES(?,1,?,?,?,?)",
                                    (identity, owner, state, json.dumps(body), identity))
        return identity

    def claim(self, identity, worker):
        with self.connection:
            sequence = self.event(worker, "task_claim", {"task_id": identity})
            self.connection.execute("UPDATE tasks SET owner=?,state='claimed',updated=? WHERE id=?",
                                    (worker, sequence, identity))

    def share(self, worker, title, path="src/parser.js"):
        body = {"summary": title, "files": [{"path": path}], "unfinished": ""}
        with self.connection:
            identity = self.event(worker, "publication", {"summary": title})
            self.connection.execute("INSERT INTO publications VALUES(?,?,1,?)",
                                    (identity, worker, json.dumps(body)))
        return identity

    def import_publication(self, identity, worker):
        with self.connection:
            self.event(worker, "apply", {"publication_id": identity, "confirmed": True})

    def append_journal(self, kind, value):
        descriptor = os.open(self.run_dir / "events.jsonl",
                             os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
        with os.fdopen(descriptor, "a") as stream:
            json.dump({"time_ms": int(time.time() * 1000), "kind": kind, "data": value}, stream)
            stream.write("\n")

    def finish(self, *, verification_required=False):
        status = "delivered" if verification_required else "complete"
        self.append_journal("claude_final", {
            "status": status,
            "delivery": {"delivered": True, "cleanup_complete": True,
                         "verification_required": verification_required,
                         "changed_paths": ["src/parser.js"], "conflicts": []},
            "checks": [{"command": "PRIVATE FINAL COMMAND"}],
        })
        self.state.update(status=status, finished=True, sequence=self.state["sequence"] + 1)
        private_json(self.run_dir / "claude.json", self.state)

    def close(self):
        self.connection.close()


class JsonStream:
    def __init__(self, process):
        self.process = process
        self.pending = b""
        self.selector = selectors.DefaultSelector()
        self.selector.register(process.stdout, selectors.EVENT_READ)

    def until(self, predicate, timeout=5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if b"\n" in self.pending:
                line, self.pending = self.pending.split(b"\n", 1)
                value = json.loads(line)
                if predicate(value):
                    return value
                continue
            if self.selector.select(max(0, deadline - time.monotonic())):
                chunk = os.read(self.process.stdout.fileno(), 1024 * 1024)
                if not chunk:
                    raise AssertionError("Observer exited before expected state")
                self.pending += chunk
        raise AssertionError("Observer did not publish expected state")

    def close(self):
        self.selector.close()


@unittest.skipUnless(RUNTIME.is_file(), "Build the runtime first: cargo build --locked")
class ClaudeBoardIntegration(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="delm-board-view-")
        self.fixture = BoardFixture(Path(self.temporary.name) / "fixture-user")
        self.children = []

    def tearDown(self):
        for process in self.children:
            if process.poll() is None:
                process.terminate()
            try:
                process.communicate(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate()
        self.fixture.close()
        self.temporary.cleanup()

    def launch(self, *extra):
        process = subprocess.Popen(self.fixture.command(*extra), env=self.fixture.environment(),
                                   stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE)
        self.children.append(process)
        return process

    def test_watch_projects_claim_share_import_and_final_without_model_work(self):
        process = self.launch("--watch")
        stream = JsonStream(process)
        self.addCleanup(stream.close)
        first = stream.until(lambda value: value["status"] == "running")
        self.assertEqual(first["schema_version"], 1)
        self.assertEqual(first["session_id"], self.fixture.session_id)
        self.assertEqual(first["agents"][0]["native_state"], "working")
        task = self.fixture.task("CSV import endpoint")
        self.fixture.claim(task, 1)
        claimed = stream.until(lambda value: any(row["id"] == task and row["owner"] == 1
                                                for row in value["tasks"]["items"]))
        publication = self.fixture.share(1, "Parser contract")
        self.fixture.import_publication(publication, 2)
        shared = stream.until(lambda value: any(row["id"] == publication and row["imported_by"] == [2]
                                               for row in value["shared"]["items"]))
        self.fixture.finish(verification_required=True)
        final = stream.until(lambda value: value["finished"])
        self.assertEqual(final["outcome"]["delivered"], True)
        self.assertEqual(final["outcome"]["verification_required"], True)
        self.assertEqual(final["outcome"]["cleanup_complete"], True)
        process.wait(timeout=3)
        self.assertEqual(process.returncode, 0)
        self.assertNotIn("PRIVATE", json.dumps([first, claimed, shared, final]))
        self.assertLessEqual(claimed["source"]["board_sequence"], shared["source"]["board_sequence"])

    def test_real_cli_pagination_and_viewing_do_not_change_coordination(self):
        for index in range(40):
            self.fixture.task(f"Requirement {index}")
        state_before = (self.fixture.run_dir / "claude.json").read_bytes()
        journal_before = (self.fixture.run_dir / "events.jsonl").read_bytes()
        sequence_before = self.fixture.connection.execute("SELECT MAX(seq) FROM events").fetchone()[0]
        first = self.fixture.read("--collection", "tasks", "--limit", 32)
        second = self.fixture.read("--collection", "tasks", "--offset", 32, "--limit", 32,
                                   "--through-sequence", first["tasks"]["through_sequence"])
        self.assertEqual(first["tasks"]["total"], 40)
        self.assertEqual(len(first["tasks"]["items"]), 32)
        self.assertEqual(len(second["tasks"]["items"]), 8)
        self.assertIsNone(second["tasks"]["next_offset"])
        self.assertEqual(state_before, (self.fixture.run_dir / "claude.json").read_bytes())
        self.assertEqual(journal_before, (self.fixture.run_dir / "events.jsonl").read_bytes())
        self.assertEqual(sequence_before, self.fixture.connection.execute("SELECT MAX(seq) FROM events").fetchone()[0])

    def test_stalled_output_does_not_hold_a_wal_read_transaction(self):
        for index in range(32):
            self.fixture.task(f"Requirement {index}", description="Explicit task detail. " * 100)
        process = self.launch("--watch", "--collection", "tasks", "--limit", 32)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            self.assertTrue(selector.select(3), "Observer did not reach its first output")
        # Leave stdout unread. Its large first snapshot fills the pipe while
        # writes and a truncating checkpoint proceed independently.
        started = time.monotonic()
        for index in range(32, 64):
            self.fixture.task(f"Additional requirement {index}")
        checkpoint = self.fixture.connection.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
        self.assertEqual(checkpoint[0], 0, "Observer retained a read transaction while output stalled")
        self.assertLess(time.monotonic() - started, 2, "Passive output blocked fixture writes")

    def test_completed_record_uses_current_binary_and_ends_without_timer(self):
        self.fixture.finish()
        self.fixture.connection.close()
        before = sorted(path.name for path in (self.fixture.run_dir / "board").iterdir())
        process = self.launch("--watch")
        output, error = process.communicate(timeout=3)
        self.assertEqual(process.returncode, 0, error.decode())
        lines = output.splitlines()
        self.assertEqual(len(lines), 1)
        self.assertEqual(json.loads(lines[0])["outcome"]["delivered"], True)
        self.assertEqual(before, sorted(path.name for path in (self.fixture.run_dir / "board").iterdir()))

    def test_unknown_version_and_wrong_session_fail_passively(self):
        self.fixture.state["version"] = 99
        private_json(self.fixture.run_dir / "claude.json", self.fixture.state)
        record_before = hashlib.sha256((self.fixture.run_dir / "claude.json").read_bytes()).digest()
        result = self.fixture.read(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Unsupported saved board view version", result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(record_before, hashlib.sha256((self.fixture.run_dir / "claude.json").read_bytes()).digest())
        self.fixture.state["version"] = 1
        self.fixture.state["session_id"] = "different-conversation"
        private_json(self.fixture.run_dir / "claude.json", self.fixture.state)
        result = self.fixture.read(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not belong to this conversation", result.stderr)

    def test_runs_directory_from_older_releases_is_viewable_with_the_real_cli(self):
        # Releases before the private storage layout created `runs` with 0755.
        # The run writer still accepts it, so the installed observer must too.
        runs = self.fixture.run_dir.parent
        self.fixture.task("Visible after an older install")
        runs.chmod(0o755)
        try:
            view = self.fixture.read()
            self.assertEqual([task["title"] for task in view["tasks"]["items"]],
                             ["Visible after an older install"])
            for mode in (0o775, 0o777):
                runs.chmod(mode)
                result = self.fixture.read(check=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertIn(f"has permissions {mode:03o}; it must not be writable by other users",
                              result.stderr)
            runs.chmod(0o755)
            self.fixture.run_dir.chmod(0o755)
            result = self.fixture.read(check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("must not be accessible to other users", result.stderr)
        finally:
            runs.chmod(0o700)
            self.fixture.run_dir.chmod(0o700)

    def test_large_publications_fit_native_transport_without_skipping_details(self):
        expected = []
        paths = [f"{index}/" + "a/" * 250 for index in range(32)]
        summary = "Shared interface for " + "界" * 500
        with self.fixture.connection:
            for _ in range(32):
                body = {"summary": summary, "files": [{"path": path} for path in paths]}
                identity = self.fixture.event(1, "publication", {"summary": summary})
                self.fixture.connection.execute("INSERT INTO publications VALUES(?,1,1,?)",
                                                (identity, json.dumps(body)))
                expected.append(identity)
        identities = []
        offset = 0
        through = None
        while offset is not None:
            arguments = ["--collection", "shared", "--limit", 32, "--offset", offset]
            if through is not None:
                arguments += ["--through-sequence", through]
            process = self.fixture.read(*arguments, check=False)
            self.assertEqual(process.returncode, 0, process.stderr)
            self.assertLessEqual(len(process.stdout.encode("utf-8")), 240 * 1024 + 1)
            page = json.loads(process.stdout)["shared"]
            self.assertEqual(page["total"], 32)
            self.assertTrue(page["items"])
            self.assertEqual(page["items"][0]["text"], summary)
            self.assertEqual(page["items"][0]["files"], paths)
            identities += [item["id"] for item in page["items"]]
            through = page["through_sequence"]
            next_offset = page["next_offset"]
            self.assertTrue(next_offset is None or next_offset > offset)
            offset = next_offset
        self.assertEqual(identities, list(reversed(expected)))

    def test_unexpected_fifo_sources_are_rejected_without_waiting_for_a_writer(self):
        record = self.fixture.run_dir / "claude.json"
        record.unlink()
        os.mkfifo(record, 0o600)
        result = self.fixture.read(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Unsafe or oversized board view source", result.stderr)
        record.unlink()
        private_json(record, self.fixture.state)
        journal = self.fixture.run_dir / "events.jsonl"
        journal.unlink()
        os.mkfifo(journal, 0o600)
        value = self.fixture.read()
        self.assertIn("journal", value["freshness"]["unavailable"])

    def test_finished_run_with_missing_evidence_exits_with_honest_freshness(self):
        self.fixture.state.update(status="complete", finished=True, sequence=2)
        private_json(self.fixture.run_dir / "claude.json", self.fixture.state)
        value = self.fixture.read("--watch")
        self.assertTrue(value["finished"])
        self.assertIsNone(value["outcome"]["delivered"])
        self.assertIn("outcome", value["freshness"]["unavailable"])

    def test_exact_item_lookup_refreshes_items_outside_the_visible_pages(self):
        identities = [self.fixture.task(f"Requirement {index}") for index in range(40)]
        selected = self.fixture.read("--collection", "tasks", "--item-id", identities[-1])["tasks"]
        self.assertEqual([item["id"] for item in selected["items"]], identities[-1:])
        self.assertEqual(selected["total"], 40)
        self.assertEqual(selected["item_id"], identities[-1])
        self.assertIsNone(selected["next_offset"])
        self.fixture.claim(identities[-1], 2)
        selected = self.fixture.read("--collection", "tasks", "--item-id", identities[-1])["tasks"]
        self.assertEqual(selected["items"][0]["owner"], 2)
        publications = [self.fixture.share(1, f"Contribution {index}") for index in range(32)]
        selected = self.fixture.read("--collection", "shared", "--item-id", publications[0])["shared"]
        self.assertEqual([item["id"] for item in selected["items"]], publications[:1])
        self.assertEqual(selected["total"], 32)
        self.assertIsNone(selected["next_offset"])
        self.assertNotEqual(self.fixture.read("--item-id", 1, check=False).returncode, 0)
        self.assertNotEqual(self.fixture.read("--collection", "tasks", "--item-id", 1,
                                             "--offset", 8, check=False).returncode, 0)


def seed_preview(fixture_home, session_id, scenario="active"):
    fixture = BoardFixture(fixture_home, session_id)
    try:
        fixture.task("CSV import endpoint", owner=1, state="claimed",
                     description="Parse and validate incoming rows using the shared field contract.")
        fixture.task("Upload interface", owner=2, state="claimed",
                     description="Display validation feedback and a preview before importing rows.")
        fixture.task("Verify the assembled flow")
        publication = fixture.share(1, "Validation contract", "src/import/contract.js")
        fixture.import_publication(publication, 2)
        with fixture.connection:
            fixture.event(2, "finding", {"text": "The preview uses the same field names as the import endpoint."})
        if scenario == "complete":
            fixture.finish()
        elif scenario == "stopped":
            fixture.state.update(status="stopped", finished=True, sequence=2,
                                 recovery={"recovery": str(fixture.run_dir / "workspace/recovery"),
                                           "cleanup_complete": True})
            private_json(fixture.run_dir / "claude.json", fixture.state)
        return {"fixture": True, "model_calls": 0, "home": str(fixture.fixture_home),
                "run_id": fixture.run_id, "session_id": fixture.session_id,
                "scenario": scenario, "run_dir": str(fixture.run_dir)}
    finally:
        fixture.close()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--seed":
        parser = argparse.ArgumentParser(description="Create sample saved facts for the real native board preview.")
        parser.add_argument("--seed", nargs=2, metavar=("FIXTURE_HOME", "SESSION_ID"), required=True)
        parser.add_argument("--scenario", choices=("active", "complete", "stopped"), default="active")
        args = parser.parse_args()
        print(json.dumps(seed_preview(*args.seed, scenario=args.scenario)))
    else:
        unittest.main()
