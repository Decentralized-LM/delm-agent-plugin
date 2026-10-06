"""Qualify exact macOS release bytes with disposable, no-model fixtures."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import shutil
import subprocess
import sys
import threading
import time

from build import HOST_PACKAGE_FILES, SOURCE, package_source
from install_support import fingerprint, package_files
from package_release import (ARCHITECTURES, CODEX_STARTUP_CASES, LIFECYCLE_CASES, source_state,
                             source_version, valid_startup_case, verify, write_json)


INHERITANCE_INPUTS = ["tests/native_inheritance.rs", "src/workers.rs", "src/worker_tools.rs", "src/compatibility.rs"]


def inheritance_source_digest(source):
    digest = hashlib.sha256()
    for name in INHERITANCE_INPUTS:
        digest.update((source / name).read_bytes())
    return digest.hexdigest()


def expected_smoke_cases():
    return [{"case": case, "passed": True, "workerCount": 2,
             "originalPreserved": True, "resultDelivered": case == "complete",
             "workspacesRemoved": True} for case in ["complete", "stop"]]


def validate_inheritance(evidence, source, runtime_hash, architecture, host_version):
    if (evidence.get("kind") != "native-inheritance" or evidence.get("model_turns") != 0
            or not all(evidence.get(key) is True for key in ["saved_skill_contents_match",
                "saved_mcp_tools_match", "native_permission_profile_match", "mcp_tool_called",
                "delm_gateway_tool_called", "parent_cli_overrides_not_exported",
                "metadata_fork_validated", "metadata_settings_match",
                "invalid_ephemeral_goal_combination_rejected", "native_plugin_initialization_validated"])
            or evidence.get("exact_live_session_parity") is not False
            or evidence.get("runtime_sha256") != runtime_hash
            or evidence.get("host_version") != host_version
            or {"aarch64": "arm64"}.get(evidence.get("architecture"), evidence.get("architecture")) != architecture
            or evidence.get("native_test_sha256") != fingerprint(source / "tests/native_inheritance.rs")["sha256"]
            or evidence.get("source_digest") != inheritance_source_digest(source)):
        raise RuntimeError("Native inheritance evidence is missing, mismatched, or overstates live parity.")
    # Keep private temporary paths and tool contents out of release metadata.
    return {"runtimeSha256": runtime_hash, "hostVersion": host_version, "architecture": architecture,
            "nativeTestSha256": evidence["native_test_sha256"], "sourceDigest": evidence["source_digest"],
            "gatewayToolCalled": True, "parentCliOverridesNotExported": True,
            "metadataForkValidated": True, "metadataSettingsMatch": True,
            "invalidEphemeralGoalCombinationRejected": True,
            "nativePluginInitializationValidated": True,
            "exactLiveSessionParity": False}


def codex_payload_digest(source):
    files = {name: fingerprint(package_source(source, name, "codex"))
             for name in HOST_PACKAGE_FILES["codex"]}
    return hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()


def validate_startup(evidence, source, runtime_hash, architecture, host_version, case):
    from verify_codex_startup import startup_source_digest

    proof = {"case": case, "passed": evidence.get("passed"), "runtimeSha256": evidence.get("runtime_sha256"),
             "hostVersion": evidence.get("host_version"), "architecture": evidence.get("architecture"),
             "modelCalls": evidence.get("model_calls"), "workerCount": evidence.get("worker_count"),
             "model": evidence.get("model"), "effort": evidence.get("effort"),
             "workerForksStarted": evidence.get("worker_forks_started"),
             "elapsedSeconds": evidence.get("elapsed_seconds"), "sourceDigest": evidence.get("source_digest"),
             "harnessSha256": evidence.get("harness_sha256"),
             "packagePayloadSha256": evidence.get("package_payload_sha256")}
    proof["architecture"] = {"aarch64": "arm64"}.get(proof["architecture"], proof["architecture"])
    for target, name in [("nativeInvocation", "native_invocation"), ("resultDelivered", "result_delivered"),
                         ("deliveryCleanupComplete", "delivery_cleanup_complete"),
                         ("installedPackageMatches", "installed_package_matches"),
                         ("nativeHostMatches", "native_host_matches"),
                         ("originalGitPreserved", "original_git_preserved"),
                         ("exactResult", "exact_result"), ("nativeCheckPassed", "native_check_passed"),
                         ("sharedPublicationObserved", "shared_publication_observed"),
                         ("completionObserved", "completion_observed"),
                         ("workspacesRemoved", "workspaces_removed"), ("ownedProcessesStopped", "owned_processes_stopped"),
                         ("authLinkRemoved", "auth_link_removed"), ("explicitFailure", "explicit_failure"),
                         ("ordinaryConversationUsable", "ordinary_conversation_usable"),
                         ("ordinaryResponseRendered", "ordinary_response_rendered")]:
        proof[target] = evidence.get(name)
    proof["originalPreserved"] = (evidence.get("original_unchanged") is True
                                   and evidence.get("original_git_preserved") is True)
    record = {"runtimeSha256": runtime_hash, "codexVersion": host_version}
    if (evidence.get("kind") != "native-codex-startup" or evidence.get("schema_version") != 1
            or evidence.get("case") != case or evidence.get("cleanup_errors") != []
            or not valid_startup_case(case, proof, record, architecture)
            or evidence.get("source_digest") != startup_source_digest(source)
            or evidence.get("harness_sha256") != fingerprint(source / "scripts/verify_codex_startup.py")["sha256"]
            or evidence.get("package_payload_sha256") != codex_payload_digest(source)):
        raise RuntimeError(f"Native production startup evidence is missing or mismatched: {case}.")
    return proof


def native_architecture(expected):
    actual = platform.machine()
    translated = subprocess.run(["sysctl", "-in", "sysctl.proc_translated"],
                                text=True, capture_output=True)
    if sys.platform != "darwin" or actual != expected or translated.stdout.strip() == "1":
        raise RuntimeError(f"Qualification requires native {expected} macOS, not emulation ({actual}).")
    return actual


def command(arguments, **kwargs):
    return subprocess.run(arguments, check=True, text=True, capture_output=True, timeout=30, **kwargs)


def smoke_case(runtime, root, mode):
    root.mkdir()
    home, project, bin_dir = root / "home", root / "original", root / "bin"
    for directory in [home / ".codex", project, bin_dir]:
        directory.mkdir(parents=True)
    project = project.resolve()
    host = bin_dir / "codex"
    shutil.copy2(SOURCE / "tests/fixtures/worker_host.py", host)
    host.chmod(0o755)
    write_json(host.with_suffix(".json"), {"mode": mode})
    (project / "source.txt").write_text("unchanged source\n")
    (root / "task.txt").write_text("Create result.txt in the private project.")
    env = {"HOME": str(home), "CODEX_HOME": str(home / ".codex"),
           "PATH": str(bin_dir) + ":/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8"}
    command(["/usr/bin/git", "init", "--quiet", "--template=", str(project)], env=env)
    before = package_files(project)
    messages, events = queue.Queue(), []
    run_id = None
    control_executable = runtime
    terminal = None
    with (root / "runtime.stderr").open("w") as stderr:
        child = subprocess.Popen([str(runtime), "run", "--project", str(project),
                                  "--task-file", str(root / "task.txt"), "--seconds", "60"],
                                 cwd=root, env=env, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=stderr, text=True)

        def read_events():
            for line in child.stdout:
                messages.put(line)
            messages.put(None)

        reader = threading.Thread(target=read_events, daemon=True)
        reader.start()

        def control(*arguments):
            result = command([str(control_executable), *arguments, "--run-id", run_id], env=env, cwd=root)
            return json.loads(result.stdout)

        try:
            deadline = time.monotonic() + 45
            while terminal is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError("Release runtime smoke timed out.")
                line = messages.get(timeout=remaining)
                if line is None:
                    raise RuntimeError("Release runtime exited without a result: " + (root / "runtime.stderr").read_text())
                event = json.loads(line)
                events.append(event)
                if event["type"] == "settings":
                    control_executable = Path(event["control_executable"])
                    if fingerprint(control_executable)["sha256"] != fingerprint(runtime)["sha256"]:
                        raise RuntimeError("Retained control runtime differs from the release binary.")
                if event.get("run_id") and run_id is None:
                    run_id = event["run_id"]
                    control("status", "--keep-alive")
                if event["type"] == "started" and mode == "wait":
                    control("stop")
                if event["type"] in ["result", "stopped", "error"]:
                    terminal = event
            child.wait(timeout=20)
            expected = "stopped" if mode == "wait" else "result"
            if child.returncode or terminal["type"] != expected:
                raise RuntimeError(f"Release runtime smoke failed: {terminal}")
            after = package_files(project)
            if expected == "result":
                if Path(terminal["path"]) != project or not (project / "result.txt").is_file():
                    raise RuntimeError("Release runtime did not deliver its output to the original project.")
                after.pop("result.txt")
                if terminal.get("details", {}).get("delivery", {}).get("delivered") is not True:
                    raise RuntimeError("Release runtime did not confirm guarded delivery.")
            else:
                recovery = terminal.get("partial_paths", [])
                if len(recovery) != 1 or not (Path(recovery[0]) / "complete.json").is_file():
                    raise RuntimeError("Stopping the release runtime did not save a recovery package.")
            if after != before:
                raise RuntimeError("Release runtime changed existing project files or Git state.")
            run_dir = home / "Library/Application Support/DeLM/runs" / run_id
            saved = json.loads((run_dir / "run.json").read_text())
            if any(Path(path).exists() for path in [saved["workspace"]["baseline"], *saved["workspace"]["workers"]]):
                raise RuntimeError("Release runtime left temporary workspaces behind.")
            wire = [json.loads(line) for line in host.with_suffix(".jsonl").read_text().splitlines()]
            workers = [event for event in wire if event.get("direction") == "in"
                       and event["message"].get("method") == "thread/start"
                       and event["message"].get("params", {}).get("ephemeral") is False]
            if len(workers) != 2:
                raise RuntimeError("Release runtime did not start exactly two task workers.")
            for pid in {event["pid"] for event in wire}:
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    continue
                raise RuntimeError("Release runtime left a fixture worker host running.")
            return {"case": "stop" if mode == "wait" else "complete", "passed": True,
                    "workerCount": 2, "originalPreserved": True,
                    "resultDelivered": expected == "result", "workspacesRemoved": True}
        finally:
            write_json(root / "events.json", events)
            if child.poll() is None:
                if run_id is not None:
                    try:
                        control("stop")
                        child.wait(timeout=20)
                    except (OSError, ValueError, subprocess.SubprocessError):
                        pass
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=10)
            reader.join(timeout=2)
            child.stdout.close()


def smoke(runtime, output, architecture, signed=False):
    native_architecture(architecture)
    runtime = runtime.resolve(strict=True)
    before = fingerprint(runtime)
    if command([str(runtime), "--version"]).stdout.strip() != "delm " + source_version(SOURCE):
        raise RuntimeError("Release runtime version differs from source.")
    if signed:
        command(["codesign", "--verify", "--strict", "--check-notarization", "-R=notarized", str(runtime)])
    output.mkdir(parents=True, exist_ok=False)
    cases = [smoke_case(runtime, output / mode, mode) for mode in ["complete", "wait"]]
    if fingerprint(runtime) != before:
        raise RuntimeError("Release runtime changed during qualification.")
    evidence = {"schema": 1, "kind": "release-runtime-smoke", "architecture": architecture,
                "macOS": platform.mac_ver()[0], "runtimeSha256": before["sha256"],
                "signedAndNotarized": signed, "modelCalls": 0, "passed": True,
                "cases": cases, **source_state(SOURCE)}
    write_json(output / "result.json", evidence)
    return evidence


def record(runtime, output, target, smoke_path, inheritance_path, lifecycle_root, startup_root):
    architecture = next((arch for arch, triple in ARCHITECTURES.items() if triple == target), None)
    if architecture is None:
        raise RuntimeError("Unsupported macOS release target.")
    native_architecture(architecture)
    runtime_hash = fingerprint(runtime)["sha256"]
    provenance = source_state(SOURCE)
    smoke_result = json.loads(smoke_path.read_text())
    if (smoke_result.get("schema") != 1 or smoke_result.get("kind") != "release-runtime-smoke"
            or smoke_result.get("cases") != expected_smoke_cases()
            or smoke_result.get("passed") is not True or smoke_result.get("runtimeSha256") != runtime_hash
            or smoke_result.get("architecture") != architecture or smoke_result.get("modelCalls") != 0
            or any(smoke_result.get(key) != value for key, value in provenance.items())):
        raise RuntimeError("Exact-runtime smoke evidence does not match this native build.")
    inheritance = json.loads(inheritance_path.read_text())
    evidence_hashes = {"smoke": fingerprint(smoke_path)["sha256"],
                       "inheritance": fingerprint(inheritance_path)["sha256"]}
    versions = set()
    fixture_hashes = set()
    for case in LIFECYCLE_CASES:
        path = lifecycle_root / case / "result.json"
        result = json.loads(path.read_text())
        if (result.get("passed") is not True or result.get("case") != case
                or result.get("real_model_calls") != 0
                or result.get("lifecycle_source_sha256") != fingerprint(SOURCE / "src/lifecycle.rs")["sha256"]):
            raise RuntimeError(f"Native lifecycle qualification failed or mismatched: {case}")
        versions.add(result["host_version"])
        fixture_hashes.add(result["fixture_sha256"])
        evidence_hashes[case] = fingerprint(path)["sha256"]
    if len(versions) != 1 or len(fixture_hashes) != 1:
        raise RuntimeError("Lifecycle qualification mixed native hosts or fixture binaries.")
    host_version = versions.pop()
    native_inheritance = validate_inheritance(inheritance, SOURCE, runtime_hash, architecture, host_version)
    native_startup = {}
    for case in CODEX_STARTUP_CASES:
        path = startup_root / case / "result.json"
        native_startup[case] = validate_startup(json.loads(path.read_text()), SOURCE,
                                               runtime_hash, architecture, host_version, case)
        evidence_hashes["startup-" + case] = fingerprint(path)["sha256"]
    revision = command(["git", "rev-parse", "HEAD"], cwd=SOURCE).stdout.strip()
    evidence = {"schema": 1, "kind": "native-release-build", "architecture": architecture,
                "target": target, "macOS": platform.mac_ver()[0], "runtimeSha256": runtime_hash,
                "sourceRevision": revision, **provenance, "passed": True, "modelCalls": 0,
                "codexVersion": host_version, "lifecycleCases": LIFECYCLE_CASES,
                "nativeInheritance": native_inheritance,
                "nativeStartup": native_startup,
                "exactLiveSessionParity": False,
                "lifecycleFixtureSha256": fixture_hashes.pop(), "evidenceSha256": evidence_hashes}
    write_json(output, evidence)
    return evidence


def verify_signed(package, reports):
    metadata = verify(package, require_qualified=True)
    if not metadata["signedAndNotarized"]:
        raise RuntimeError("Signed qualification requires a signed package.")
    runtime_hash = metadata["files"]["bin/delm"]["sha256"]
    for arch in ARCHITECTURES:
        report = json.loads((reports / ("signed-qualification-" + arch) / "result.json").read_text())
        if (report.get("kind") != "release-runtime-smoke" or report.get("architecture") != arch
                or report.get("runtimeSha256") != runtime_hash or report.get("passed") is not True
                or report.get("signedAndNotarized") is not True or report.get("modelCalls") != 0
                or report.get("sourceDirty") is not False
                or report.get("runtimeSourcesSha256") != metadata["runtimeSourcesSha256"]
                or report.get("cases") != expected_smoke_cases()):
            raise RuntimeError(f"Signed runtime qualification does not match the package: {arch}")
    command(["codesign", "--verify", "--strict", "--check-notarization", "-R=notarized",
             str(package / "plugins/delm/bin/delm")])
    return {"kind": "signed-release-qualified", "architecture": "+".join(ARCHITECTURES),
            "runtimeSha256": runtime_hash, "sourceDirty": False, "passed": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="operation", required=True)
    smoke_parser = subparsers.add_parser("smoke")
    smoke_parser.add_argument("--runtime", type=Path, required=True)
    smoke_parser.add_argument("--out", type=Path, required=True)
    smoke_parser.add_argument("--architecture", choices=ARCHITECTURES, required=True)
    smoke_parser.add_argument("--signed", action="store_true")
    record_parser = subparsers.add_parser("record")
    record_parser.add_argument("--runtime", type=Path, required=True)
    record_parser.add_argument("--out", type=Path, required=True)
    record_parser.add_argument("--target", choices=ARCHITECTURES.values(), required=True)
    record_parser.add_argument("--smoke", type=Path, required=True)
    record_parser.add_argument("--inheritance", type=Path, required=True)
    record_parser.add_argument("--lifecycle-root", type=Path, required=True)
    record_parser.add_argument("--startup-root", type=Path, required=True)
    verify_parser = subparsers.add_parser("verify-signed")
    verify_parser.add_argument("--package", type=Path, required=True)
    verify_parser.add_argument("--reports", type=Path, required=True)
    args = parser.parse_args()
    if args.operation == "smoke":
        evidence = smoke(args.runtime, args.out.resolve(), args.architecture, args.signed)
    elif args.operation == "record":
        evidence = record(args.runtime, args.out, args.target, args.smoke, args.inheritance,
                          args.lifecycle_root, args.startup_root)
    else:
        evidence = verify_signed(args.package, args.reports)
    print(json.dumps({key: evidence[key] for key in ["kind", "architecture", "runtimeSha256", "sourceDirty", "passed"]}))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, queue.Empty) as error:
        sys.exit(str(error))
