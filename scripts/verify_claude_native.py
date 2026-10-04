#!/usr/bin/env python3
"""Explicit, bounded native Claude qualification using the caller's existing setup.

This is a manual real-model check, never an ordinary CI test. It installs nothing,
changes no Claude settings, and grants no tool permissions. Evidence stays in the
new --out directory, including failed runs and their native recovery references.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import selectors
import shutil
import subprocess
import time

from package_release import source_state
from build import claude_adapter_digest
from qualify_release import native_architecture


SOURCE = Path(__file__).resolve().parent.parent
TASK = """Build a tiny dependency-free Node utility to qualify collaboration, not a full application.
Create slug.mjs exporting slugify(text): lowercase ASCII, trim, replace runs of non-alphanumeric characters with one hyphen, and remove edge hyphens.
Create stats.mjs exporting summarize(values): return {count,min,max,mean}, without mutating input; empty input returns count 0 and null min/max/mean.
Create demo.mjs which imports both modules and prints exactly the JSON object {"slug":"hello-delm","count":3,"min":2,"max":6,"mean":4} for the sample ' Hello, DeLM! ' and [2,4,6].
Add one small test.mjs using node:test with focused cases for both functions. Both DeLM peers must implement and publish a distinct useful module; one then imports its peer's contribution and checks the combined demo. Keep the split small and use the shared queue naturally.
Do not modify KEEP.txt, STAGED.txt, CLAUDE.md, or the Git index. Do not install dependencies, research external libraries, add a UI, create servers, or expand testing beyond these tiny functions and their combined output. Finish as soon as the focused check passes.
"""


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def sanitized(value):
    if isinstance(value, dict):
        if value.get("type") in {"thinking", "redacted_thinking"}:
            return {"type": value["type"], "omitted": True}
        return {key: ("[redacted]" if key.lower() in {
            "token", "ticket", "_delm", "access_token", "refresh_token", "id_token",
            "authorization", "api_key", "apikey", "email", "account", "account_info",
        } else sanitized(item)) for key, item in value.items()}
    if isinstance(value, list):
        return [sanitized(item) for item in value]
    return value


def read_json(path, fallback):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return fallback


def read_journal(run):
    try:
        return [json.loads(line) for line in (run / "events.jsonl").read_text().splitlines() if line]
    except (OSError, ValueError):
        return []


def probe_source(output):
    return """const OUTPUT = __OUTPUT__;
let rows = []; let writes = Promise.resolve(); let first = new Set();
function record($, row) {
  rows.push(row);
  const text = JSON.stringify(rows, null, 2);
  writes = writes.then(() => $.fs.write(OUTPUT, text));
  return writes;
}
export function register(on) {
  on('prompt.submit', async ($, e, next) => {
    if (e.origin.kind === 'plugin' && e.text.startsWith('DeLM completed its native handoff.')) {
      await record($, {event:'handoff_submitted', at:await $.clock.now()});
    }
    return next(e);
  });
  on('turn.start', async ($, e, next) => {
    if (e.text.startsWith('DeLM completed its native handoff.')
        || e.text.startsWith('The delm plugin sent a message:\\nDeLM completed its native handoff.')) {
      await record($, {event:'handoff_started', at:await $.clock.now(), turn:e.turnId});
    }
    return next(e);
  });
  on('session.start', async ($, e, next) => {
    await record($, {event:'session', at:await $.clock.now(), version:await $.session.version(),
      session:await $.session.id(), cwd:await $.session.cwd(), model:await $.session.model()});
    return next(e);
  });
  on('process.spawn', async function* ($, e, next) {
    const observe = e.argv.slice(-2).join(' ') === 'claude serve';
    const stream = next(e); let buffer = '';
    for (;;) {
      const item = await stream.next();
      if (item.done) return item.value;
      if (observe && item.value.stream === 'stdout') {
        buffer += item.value.text;
        const lines = buffer.split('\\n'); buffer = lines.pop();
        for (const line of lines) {
          try {
            const event = JSON.parse(line);
            if (event.type === 'ready') await record($, {event:'ready', at:await $.clock.now(),
              run_id:event.run_id, run_dir:event.run_dir, workers:event.workers, revision:event.revision});
          } catch {}
        }
      }
      yield item.value;
    }
  });
  on('agent.spawn', async ($, e, next) => {
    if (!e.parentAgentId) await record($, {event:'parent_tools', at:await $.clock.now(),
      phase:'before_spawn', tools:(await $.tool.list()).map(tool=>tool.name).sort()});
    const result = await next(e);
    await record($, {event:'spawn', at:await $.clock.now(), agent:result.agentId,
      parent:e.parentAgentId, fork:e.fork, cwd:e.cwd, permissionMode:e.permissionMode, model:result.model,
      denied:result.deny});
    if (!e.parentAgentId) await record($, {event:'parent_tools', at:await $.clock.now(),
      phase:'after_spawn', agent:result.agentId, tools:(await $.tool.list()).map(tool=>tool.name).sort()});
    return result;
  });
  on('turn.step', async function* ($, e, next) {
    await record($, {event:'model_step', at:await $.clock.now(), agent:e.agentId, turn:e.turnId, index:e.index});
    if (!e.agentId) await record($, {event:'parent_tools', at:await $.clock.now(),
      phase:'model_step', tools:(await $.tool.list()).map(tool=>tool.name).sort()});
    const key = e.agentId || 'parent';
    if (!first.has(key)) {
      first.add(key);
      await record($, {event:'first_step', at:await $.clock.now(), agent:e.agentId,
        turn:e.turnId, model:e.model, effort:e.effort, cwd:await $.session.cwd(),
        tools:(await $.tool.list()).map(tool=>tool.name).sort()});
    }
    return yield* next(e);
  });
  on('tool.call', async ($, e, next) => {
    const observed = e.agentId && (e.tool === 'Bash' || e.tool.startsWith('mcp__plugin_delm_delm__'));
    if (observed) await record($, {event:'tool_start', at:await $.clock.now(), agent:e.agentId,
      call:e.tool_use_id, tool:e.tool, cwd:await $.session.cwd(), command:e.command});
    const result = await next(e);
    if (observed) await record($, {event:'tool_end', at:await $.clock.now(), agent:e.agentId,
      call:e.tool_use_id, tool:e.tool, ref:result.ref, isError:result.isError === true,
      denied:result.deny, background_task:result.result?.backgroundTaskId});
    return result;
  });
  on('turn.complete', async ($, e, next) => {
    await record($, {event:'turn_complete', at:await $.clock.now(), agent:e.agentId,
      turn:e.turnId, reason:e.reason, aborted:e.isAborted, answer:e.answer});
    return next(e);
  });
}
""".replace("__OUTPUT__", json.dumps(str(output)))


def prepare(output):
    project = output / "project"
    project.mkdir()
    (project / "KEEP.txt").write_text("Unrelated original project content.\n")
    (project / "CLAUDE.md").write_text(
        "Native DeLM fixture. Preserve KEEP.txt, STAGED.txt, CLAUDE.md, and Git metadata.\n"
        "Context marker: DELM_NATIVE_PROJECT_CONTEXT_7K4.\n")
    (project / "STAGED.txt").write_text("Already staged user content.\n")
    subprocess.run(["git", "init", "-q", str(project)], check=True)
    subprocess.run(["git", "add", "KEEP.txt", "CLAUDE.md", "STAGED.txt"], cwd=project, check=True)
    original = {name: digest(project / name) for name in ["KEEP.txt", "STAGED.txt", "CLAUDE.md", ".git/index"]}
    probe = output / "probe-plugin"
    (probe / ".claude-plugin").mkdir(parents=True)
    (probe / "hooks").mkdir()
    save(probe / ".claude-plugin/plugin.json", {"name": "delm-native-qualification", "version": "1.0.0"})
    save(probe / "hooks/hooks.json", {"modules": ["./observe.js"]})
    (probe / "hooks/observe.js").write_text(probe_source(output / "native-events.json"))
    (output / "task.txt").write_text(TASK)
    return project, probe, original


def inspect(project, original, native, journal, run_dir=None):
    final = next((row["data"] for row in reversed(journal) if row.get("kind") == "claude_final"), None)
    prepared = next((row["data"] for row in journal if row.get("kind") == "claude_prepared"), {})
    workers = prepared.get("workers", [])
    publications = [row["data"] for row in journal if row.get("kind") == "claude_coordination"
                    and row["data"].get("tool") == "delm_publish"
                    and row["data"].get("arguments", {}).get("paths")
                    and not row["data"].get("response", {}).get("error")]
    publishers = sorted({row["worker"] for row in publications})
    parent_step = next((row for row in native if row.get("event") == "first_step" and not row.get("agent")), None)
    peer_steps = [row for row in native if row.get("event") == "first_step" and row.get("agent")]
    spawn = [row for row in native if row.get("event") == "spawn" and row.get("agent") and not row.get("parent")]
    parent_tools = [row for row in native if row.get("event") == "parent_tools"]
    spawn_by_agent = {row["agent"]: row for row in spawn}
    pool_witnesses = []
    for peer in peer_steps:
        fork = spawn_by_agent.get(peer["agent"])
        before = max((sample for sample in parent_tools
                      if fork and sample.get("phase") == "before_spawn" and sample["at"] <= fork["at"]),
                     key=lambda sample: sample["at"], default=None)
        if before is None:
            pool_witnesses.append(None)
            continue
        missing = sorted(set(before["tools"]) - set(peer["tools"]))
        extra = sorted(set(peer["tools"]) - set(before["tools"]))
        # Native MCP discovery is asynchronous. Record extras honestly rather than
        # treating a later parent snapshot as proof of the initial fork inventory.
        later = min((sample for sample in parent_tools if sample["at"] >= fork["at"]
                     and set(extra).issubset(sample["tools"])), key=lambda sample: sample["at"], default=None)
        pool_witnesses.append({"agent": peer["agent"], "fork_at": fork["at"], "peer_at": peer["at"],
                               "parent_at": before["at"], "parent_phase": before["phase"],
                               "tool_count": len(peer["tools"]), "missing_parent_tools": missing,
                               "asynchronously_connected_tools": extra,
                               "extra_parent_witness_at": later["at"] if extra and later else None,
                               "passed": not missing and (not extra or later is not None)})
    baseline = run_dir / "workspace/baseline" if run_dir else None
    result = {
        "final": final,
        "both_workers_published_files": publishers == [1, 2],
        "publications": [{"worker": row["worker"], "paths": row["arguments"]["paths"]} for row in publications],
        "exactly_two_native_forks": len(spawn) == len(spawn_by_agent) == 2
            and all(row.get("fork") is True and not row.get("denied") for row in spawn)
            and len(peer_steps) == 2 and {row["agent"] for row in peer_steps} == set(spawn_by_agent),
        "native_worker_models": [row.get("model") for row in peer_steps],
        "matching_native_models": bool(parent_step and parent_step.get("model")) and len(peer_steps) == 2
            and all(row.get("model") == parent_step.get("model") for row in peer_steps),
        "native_permission_modes": [row.get("permissionMode") for row in spawn],
        "matching_native_tool_pools": len(peer_steps) == 2 and all(pool_witnesses)
            and all(witness["passed"] for witness in pool_witnesses)
            and peer_steps[0].get("tools") == peer_steps[1].get("tools"),
        "tool_pool_parent_witnesses": pool_witnesses,
        "native_observed_cwds": [row.get("cwd") for row in peer_steps],
        "matching_native_worker_cwds": len(workers) == len(peer_steps) == 2
            and sorted(row.get("cwd", "") for row in peer_steps) == sorted(worker["cwd"] for worker in workers)
            and all(row.get("cwd") == spawn_by_agent.get(row["agent"], {}).get("cwd") for row in peer_steps),
        "worker_start_ms_from_parent_model": [row["at"] - parent_step["at"] for row in peer_steps] if parent_step else [],
        "original_files_and_index_unchanged": all((project / name).is_file() and digest(project / name) == value
                                                   for name, value in original.items()),
        "worker_trees_removed": len(workers) == 2 and all(not Path(worker["cwd"]).exists() for worker in workers)
            and baseline is not None and not baseline.exists()
            and bool(final and final.get("delivery", {}).get("cleanup_complete") is True),
    }
    result["passed"] = bool(final and final.get("delivery", {}).get("delivered") is True
                            and all(result[key] for key in ["both_workers_published_files", "exactly_two_native_forks",
                                "matching_native_tool_pools", "matching_native_models", "matching_native_worker_cwds",
                                "original_files_and_index_unchanged", "worker_trees_removed"]))
    return result


def finalize_report(report, plugin, metadata):
    """Qualify only unchanged inputs and a naturally completed native session."""
    try:
        unchanged = (digest(plugin / "bin/delm") == metadata["binary_sha256"]
                     and claude_adapter_digest(plugin) == metadata["adapter_sha256"]
                     and digest(Path(__file__)) == metadata["fixture_sha256"]
                     and source_state(SOURCE)["runtimeSourcesSha256"] == metadata["runtime_sources_sha256"])
    except (OSError, ValueError, RuntimeError):
        unchanged = False
    report["sources_and_runtime_unchanged"] = unchanged
    report["passed"] = bool(report["passed"] and report.get("delivered_output_checked") is True
                            and report.get("native_final_handoff_completed") is True
                            and report["host_version_after"] == metadata["host_version_before"]
                            and not report["deadline_stop_requested"] and not report["forced_host_stop"]
                            and unchanged)
    return report


def handoff_finished(native):
    starts = {row["turn"] for row in native if row.get("event") == "handoff_started"}
    return any(row.get("event") == "turn_complete" and not row.get("agent")
               and row.get("turn") in starts and row.get("reason") == "answer"
               and not row.get("aborted") for row in native)


def handoff_wait_finished(native, observed_at, now):
    return handoff_finished(native) or now - observed_at >= 25


def qualification_record(project, native, report, metadata):
    if not report["passed"]:
        return None
    return {
        "schema": 1, "kind": "claude-native-qualification", "architecture": metadata["architecture"],
        "hostVersion": metadata["host_version_before"], "runtimeSha256": metadata["binary_sha256"],
        "runtimeSourcesSha256": metadata["runtime_sources_sha256"], "fixtureSha256": metadata["fixture_sha256"],
        "adapterSha256": metadata["adapter_sha256"],
        "passed": True, "workerCount": 2, "exactlyTwoNativeForks": report["exactly_two_native_forks"],
        "bothWorkersPublishedFiles": report["both_workers_published_files"],
        "matchingNativeToolPools": report["matching_native_tool_pools"],
        "originalPreserved": report["original_files_and_index_unchanged"],
        "workspacesRemoved": report["worker_trees_removed"],
        "deliveredOutputChecked": report["delivered_output_checked"],
        "outputProof": {name: digest(project / name) for name in ["slug.mjs", "stats.mjs", "demo.mjs", "test.mjs"]},
        "modelCalls": sum(row.get("event") == "model_step" for row in native),
    }


def check_delivery(project, output):
    try:
        check = subprocess.run(["node", "--input-type=module", "-e",
            "import assert from 'node:assert/strict'; import {slugify} from './slug.mjs'; import {summarize} from './stats.mjs';"
            "assert.equal(slugify(' Hello, DeLM! '),'hello-delm'); assert.equal(slugify('---'),'');"
            "const xs=[2,4,6]; assert.deepEqual(summarize(xs),{count:3,min:2,max:6,mean:4});"
            "assert.deepEqual(xs,[2,4,6]); assert.deepEqual(summarize([]),{count:0,min:null,max:null,mean:null});"],
            cwd=project, text=True, capture_output=True, timeout=10)
        demo = subprocess.run(["node", "demo.mjs"], cwd=project, text=True, capture_output=True, timeout=10)
        evidence = {"exit": check.returncode, "stdout": check.stdout, "stderr": check.stderr,
                    "demo_exit": demo.returncode, "demo_stdout": demo.stdout, "demo_stderr": demo.stderr}
        try:
            correct = json.loads(demo.stdout) == {"slug": "hello-delm", "count": 3, "min": 2, "max": 6, "mean": 4}
        except ValueError:
            correct = False
        passed = check.returncode == 0 and demo.returncode == 0 and correct
    except (OSError, subprocess.TimeoutExpired) as error:
        evidence = {"error": str(error), "passed": False}
        passed = False
    save(output / "delivered-check.json", evidence)
    return passed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plugin", type=Path, required=True, help="Built and staged Claude plugin directory")
    parser.add_argument("--out", type=Path, required=True, help="New evidence directory; must not already exist")
    parser.add_argument("--authorize-model-use", action="store_true", help="Explicitly permit this bounded real-model fixture")
    parser.add_argument("--timeout", type=int, default=150, help="Work deadline in seconds, at most 150")
    parser.add_argument("--claude", default="claude")
    args = parser.parse_args()
    if not args.authorize_model_use:
        parser.error("This manual fixture requires explicit --authorize-model-use.")
    if not 15 <= args.timeout <= 150:
        parser.error("--timeout must be between 15 and 150 seconds.")
    architecture = platform.machine()
    if architecture not in {"arm64", "x86_64"}:
        parser.error("Qualification requires native arm64 or x86_64 macOS.")
    native_architecture(architecture)
    plugin = args.plugin.resolve()
    if not all((plugin / name).is_file() for name in ["bin/delm", ".claude-plugin/plugin.json", "hooks/delm.js", "hooks/worker.md"]):
        parser.error("--plugin must name the complete staged Claude payload.")
    output = args.out.resolve()
    if output.exists():
        parser.error("--out must be a new directory; previous evidence is never overwritten.")
    output.mkdir(parents=True, mode=0o700)
    project, probe, original = prepare(output)
    claude = shutil.which(args.claude)
    if not claude:
        parser.error("Claude Code CLI is not available.")
    version = subprocess.check_output([claude, "--version"], text=True, timeout=10).strip()
    source_files = sorted(set(SOURCE.glob("src/**/*.rs")) | set(SOURCE.glob("hosts/claude/**/*.js")) | {SOURCE / "plugin/worker.md"})
    metadata = {"host_version_before": version, "plugin": str(plugin), "binary_sha256": digest(plugin / "bin/delm"),
                "runtime_sources_sha256": source_state(SOURCE)["runtimeSourcesSha256"],
                "fixture_sha256": digest(Path(__file__)), "architecture": architecture,
                "adapter_sha256": claude_adapter_digest(plugin),
                "source_sha256": {str(path.relative_to(SOURCE)): digest(path) for path in source_files},
                "task_sha256": hashlib.sha256(TASK.encode()).hexdigest(), "permissions": "unchanged native user setup",
                "headless_fork_capability": "CLAUDE_CODE_FORK_SUBAGENT=1 (interactive native fork parity)"}
    save(output / "metadata.json", metadata)
    env = os.environ.copy()
    env["CLAUDE_CODE_FORK_SUBAGENT"] = "1"
    argv = [claude, "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
            "--plugin-dir", str(plugin), "--plugin-dir", str(probe)]
    started = time.monotonic()
    rows, run_dir, stop_sent, forced_host_stop = [], None, False, False
    final_observed_at = None
    output_buffer = b""
    with (output / "stderr.txt").open("w") as stderr, (output / "transcript.jsonl").open("w") as transcript:
        process = subprocess.Popen(argv, cwd=project, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=stderr, text=True, bufsize=1)
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)

        def send(text):
            process.stdin.write(json.dumps({"type": "user", "message": {"role": "user", "content": text}}) + "\n")
            process.stdin.flush()

        send("/delm:run " + TASK)
        try:
            while process.poll() is None:
                for key, _ in selector.select(timeout=0.2):
                    output_buffer += os.read(key.fileobj.fileno(), 65536)
                    lines = output_buffer.split(b"\n")
                    output_buffer = lines.pop()
                    for line in lines:
                        try:
                            row = sanitized(json.loads(line))
                        except ValueError:
                            continue
                        rows.append(row)
                        transcript.write(json.dumps(row) + "\n")
                    transcript.flush()
                native = read_json(output / "native-events.json", [])
                ready = next((row for row in native if row.get("event") == "ready"), None)
                if ready:
                    run_dir = Path(ready["run_dir"])
                journal = read_journal(run_dir) if run_dir else []
                final = next((row for row in reversed(journal) if row.get("kind") == "claude_final"), None)
                if final:
                    if final_observed_at is None:
                        final_observed_at = time.monotonic()
                    if handoff_wait_finished(native, final_observed_at, time.monotonic()):
                        break
                if not run_dir and any(row.get("type") == "result" for row in rows):
                    break
                elapsed = time.monotonic() - started
                if elapsed >= args.timeout and not stop_sent and not final:
                    send("/delm-stop")
                    stop_sent = True
                if elapsed >= args.timeout + 15 and not final:
                    break
        finally:
            selector.close()
            process.stdin.close()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                forced_host_stop = True
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        pass  # Retain the failed report even if the OS has not reaped the owned host.
    native = read_json(output / "native-events.json", [])
    journal = read_journal(run_dir) if run_dir else []
    if run_dir:
        save(output / "runtime-events.json", sanitized(journal))
    report = inspect(project, original, native, journal, run_dir)
    report["native_final_handoff_completed"] = handoff_finished(native)
    report["passed"] = report["passed"] and report["native_final_handoff_completed"]
    try:
        host_version_after = subprocess.check_output([claude, "--version"], text=True, timeout=10).strip()
    except (OSError, subprocess.SubprocessError) as error:
        host_version_after = None
        report["host_version_error"] = str(error)
    report.update({"elapsed_seconds": round(time.monotonic() - started, 3), "deadline_stop_requested": stop_sent,
                   "forced_host_stop": forced_host_stop,
                   "claude_exit": process.returncode, "run_dir": str(run_dir) if run_dir else None,
                   "host_version_after": host_version_after,
                   "reported_cost_usd": [row.get("total_cost_usd") for row in rows if row.get("type") == "result"]})
    if report["passed"]:
        report["delivered_output_checked"] = check_delivery(project, output)
        report["passed"] = report["delivered_output_checked"]
    finalize_report(report, plugin, metadata)
    save(output / "report.json", report)
    record = qualification_record(project, native, report, metadata)
    if record:
        save(output / "qualification.json", record)
    print(json.dumps({"passed": report["passed"], "report": str(output / "report.json"), "elapsed_seconds": report["elapsed_seconds"]}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
