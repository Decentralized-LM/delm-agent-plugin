"""Build the shared DeLM runtime and stage self-contained native host plugins."""

import os
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import uuid

from install_support import fingerprint, locked, package_files
from maintenance import assert_maintenance_safe
from dependency_notices import FILENAME as DEPENDENCY_NOTICES, validate as validate_dependency_notices


SOURCE = Path(__file__).resolve().parent.parent
PACKAGE_FILES = (
    ".codex-plugin/plugin.json", "skills/run/SKILL.md",
    "skills/run/agents/openai.yaml", "hooks/hooks.json", "LICENSE", "NOTICE", DEPENDENCY_NOTICES,
)
CLAUDE_PACKAGE_FILES = (
    ".claude-plugin/plugin.json", ".mcp.json", "skills/run/SKILL.md", "hooks/hooks.json",
    "hooks/delm.js", "hooks/protocol.js", "hooks/board-view.js", "hooks/board-render.js",
    "hooks/worker.md", "LICENSE", "NOTICE", DEPENDENCY_NOTICES,
)
PI_PACKAGE_FILES = (
    "package.json", "README.md", "extensions/delm.ts", "extensions/delm-worker.ts",
    "lib/protocol.ts", "lib/rpc.ts", "lib/bridge.ts", "lib/control.ts", "lib/board.ts",
    "worker.md", "LICENSE", "NOTICE", DEPENDENCY_NOTICES,
)
HOST_PACKAGE_FILES = {"codex": PACKAGE_FILES, "claude": CLAUDE_PACKAGE_FILES, "pi": PI_PACKAGE_FILES}
PUBLISHED_NAMES = {"codex": "plugin", "claude": "plugin-claude", "pi": "plugin-pi"}


def package_source(source, name, host):
    if host == "claude" and name == "hooks/worker.md":
        return source / "plugin/worker.md"
    if host == "pi" and name == "worker.md":
        return source / "plugin/worker.md"
    if host in {"claude", "pi"} and name not in {"LICENSE", "NOTICE", DEPENDENCY_NOTICES}:
        return source / f"hosts/{host}" / name
    return source / name


def payload_digest(package):
    files = {name: value for name, value in package_files(package).items() if name != "bin/delm"}
    return hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()



def claude_adapter_digest(package):
    """Bind executed adapter resources; publication supplies only repository metadata."""
    observed = package_files(package)
    missing = set(CLAUDE_PACKAGE_FILES) - set(observed)
    if missing:
        raise RuntimeError("Claude adapter resources are missing: " + ", ".join(sorted(missing)))
    # Claude writes editor types and tsconfig after loading a plugin. Those are
    # not shipped or executed; bind the explicit package resources, not caches.
    # Full release verification separately rejects additions to the shipped tree.
    files = {name: observed[name] for name in CLAUDE_PACKAGE_FILES}
    manifest_name = ".claude-plugin/plugin.json"
    manifest = json.loads((package / manifest_name).read_text())
    manifest.pop("repository", None)
    files[manifest_name]["sha256"] = hashlib.sha256(
        json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    return hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()


def validate_claude_runtime(runtime):
    """Probe bundled host interfaces without invoking tools or starting a session."""
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-03-26", "capabilities": {},
            "clientInfo": {"name": "delm-package-validation", "version": "1"}}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
    ]
    with tempfile.TemporaryDirectory(prefix="delm-runtime-probe-") as temporary:
        env = {"HOME": temporary, "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LANG": "en_US.UTF-8"}
        result = subprocess.run([str(runtime.resolve()), "claude", "mcp"],
                                input="".join(json.dumps(value) + "\n" for value in requests),
                                cwd=temporary, env=env, text=True, capture_output=True, timeout=15)
        view = subprocess.run([str(runtime.resolve()), "claude", "view", "--help"],
                              cwd=temporary, env=env, text=True, capture_output=True, timeout=15)
    try:
        replies = [json.loads(line) for line in result.stdout.splitlines()]
        valid = (result.returncode == 0 and len(replies) == 2
                 and all(isinstance(reply, dict) and reply.get("jsonrpc") == "2.0" for reply in replies)
                 and replies[0].get("id") == 1 and replies[1].get("id") == 2
                 and replies[0].get("result", {}).get("serverInfo", {}).get("name") == "delm"
                 and {"delm_status", "delm_complete", "delm_service"}.issubset({
                     tool.get("name") for tool in replies[1].get("result", {}).get("tools", [])
                     if isinstance(tool, dict)}))
    except (ValueError, TypeError, AttributeError):
        valid = False
    if not valid:
        raise RuntimeError("The prebuilt runtime does not provide the native Claude MCP transport; rebuild DeLM first.")
    if view.returncode != 0 or not all(flag in view.stdout for flag in (
            "--run-id", "--session-id", "--watch", "--interval-ms", "--collection", "--through-sequence", "--item-id")):
        raise RuntimeError("The prebuilt runtime does not provide the Claude board observer; rebuild DeLM first.")


def validate_claude_package(package, claude="claude"):
    """Native static validation only: no session, login, tools, or model calls."""
    executable = shutil.which(str(claude))
    if not executable:
        raise RuntimeError("Claude Code CLI is required to validate the Claude plugin; select it with --claude PATH.")
    executable = str(Path(executable).resolve())
    package = package.resolve()
    before = package_files(package)
    with tempfile.TemporaryDirectory(prefix="delm-claude-validate-") as temporary:
        home = Path(temporary)
        config = home / "config"
        config.mkdir()
        env = {"HOME": str(home), "CLAUDE_CONFIG_DIR": str(config),
               "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LANG": "en_US.UTF-8"}

        def invoke(*args):
            try:
                return subprocess.run([executable, *args], cwd=home, env=env, check=True,
                                      text=True, capture_output=True, timeout=60).stdout
            except subprocess.CalledProcessError as error:
                raise RuntimeError("Claude native plugin validation failed: " + (error.stderr or error.stdout).strip()) from error

        version = invoke("--version").strip()
        raw = invoke("plugin", "validate", str(package), "--strict", "--json")
        result = json.loads(raw)
        if (not isinstance(result, dict) or result.get("success") is not True or result.get("strict") is not True
                or not version or invoke("--version").strip() != version):
            raise RuntimeError("Claude native plugin validation failed or mixed host versions.")
    if package_files(package) != before:
        raise RuntimeError("Claude plugin files changed during native validation.")
    return {"kind": "claude-plugin-validation", "hostVersion": version, "strict": True,
            "passed": True, "modelCalls": 0, "payloadSha256": payload_digest(package)}


# `node --check` on a .ts file only confirms the file exists; the types must be
# stripped first, which fails on malformed TypeScript, and the result checked
# as JavaScript.
TYPESCRIPT_CHECK = '''
const {stripTypeScriptTypes} = require("node:module");
const {readFileSync, writeFileSync, mkdtempSync, rmSync} = require("node:fs");
const {join} = require("node:path");
const {tmpdir} = require("node:os");
const {spawnSync} = require("node:child_process");
const source = process.argv[1];
const stripped = stripTypeScriptTypes(readFileSync(source, "utf8"), {mode: "strip"});
const dir = mkdtempSync(join(tmpdir(), "delm-ts-check-"));
try {
  const out = join(dir, "checked.mjs");
  writeFileSync(out, stripped);
  const result = spawnSync(process.execPath, ["--check", out], {encoding: "utf8"});
  if (result.status !== 0) { process.stderr.write(result.stderr.replace(out, source)); process.exit(1); }
} finally { rmSync(dir, {recursive: true, force: true}); }
'''


def check_typescript(path):
    result = subprocess.run(["node", "-e", TYPESCRIPT_CHECK, str(path)], capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"Invalid TypeScript in {path.name}: {(result.stderr or result.stdout).strip()[:400]}")


def validate_pi_package(package):
    """Pi loads TypeScript through jiti at runtime; check the manifest and syntax offline."""
    manifest = json.loads((package / "package.json").read_text())
    declared = manifest.get("pi", {}).get("extensions", [])
    if sorted(declared) != ["./extensions/delm-worker.ts", "./extensions/delm.ts"]:
        raise RuntimeError("The Pi package manifest must declare exactly the two DeLM extensions.")
    if shutil.which("node") is None:
        raise RuntimeError("Node.js is required to validate the Pi package.")
    for name in PI_PACKAGE_FILES:
        if name.endswith(".ts"):
            check_typescript(package / name)
    # The extensions call `delm pi serve|mcp|view|recover`; a runtime without the Pi host must not stage.
    probe = subprocess.run([str(package / "bin" / "delm"), "pi", "view", "--help"], capture_output=True, text=True)
    if probe.returncode != 0 or "--run-id" not in probe.stdout + probe.stderr:
        raise RuntimeError("The staged runtime does not provide the Pi host commands: " + (probe.stderr or probe.stdout).strip()[:300])
    return {"kind": "pi-package-validation", "extensions": declared}


def stage_package(source, runtime, package, host="codex"):
    """Stage only runtime resources, never a checkout or its local artifacts."""
    if host not in HOST_PACKAGE_FILES:
        raise RuntimeError(f"Unknown host: {host}")
    validate_dependency_notices(source)
    fingerprint(runtime)
    (package / "bin").mkdir(parents=True)
    shutil.copy2(runtime, package / "bin/delm")
    (package / "bin/delm").chmod(0o755)
    for name in HOST_PACKAGE_FILES[host]:
        original = package_source(source, name, host)
        for component in original.relative_to(source).parents:
            if (source / component).is_symlink():
                raise RuntimeError(f"Refusing a linked package directory: {source / component}")
        fingerprint(original)
        destination = package / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(original, destination)


def build(source, prebuilt=None, host="codex", claude="claude"):
    if host not in {*HOST_PACKAGE_FILES, "all"}:
        raise RuntimeError(f"Unknown host: {host}")
    if prebuilt is None and shutil.which("cargo") is None:
        raise RuntimeError("Rust/Cargo is required to build DeLM; install the repository's Rust toolchain first.")
    build_root = source / ".build"
    build_root.mkdir(exist_ok=True)
    with locked(build_root / "plugin-build.lock"):
        target = build_root / "runtime-target"
        if prebuilt is None:
            subprocess.run([
                "cargo", "build", "--locked", "--release", "--bin", "delm",
                "--manifest-path", str(source / "Cargo.toml"), "--target-dir", str(target),
            ], cwd=source, check=True)
        runtime = prebuilt or target / "release/delm"
        staged = []
        try:
            for selected in HOST_PACKAGE_FILES if host == "all" else [host]:
                package = Path(tempfile.mkdtemp(prefix=f"plugin-{selected}-stage.", dir=build_root))
                staged.append((selected, package, build_root / PUBLISHED_NAMES[selected]))
                stage_package(source, runtime, package, selected)
                subprocess.run([str(package / "bin/delm"), "--version"], check=True)
                if selected == "claude":
                    validate_claude_runtime(package / "bin/delm")
                    validate_claude_package(package, claude)
                if selected == "pi":
                    validate_pi_package(package)
                published = build_root / PUBLISHED_NAMES[selected]
                if os.path.lexists(published) and (not published.is_dir() or published.is_symlink()):
                    raise RuntimeError(f"Refusing to replace an unrelated path: {published}")
            # Validate every requested package before making any new package active.
            # Codex uses a copied native cache; staging it is offline. Claude Code and
            # Pi load their package directories in place (Pi re-reads bin/delm,
            # worker.md and the worker extension during a run), so protect those
            # adapters before activation. Unrelated known package roots are safe.
            for selected, package, published in staged:
                if selected in {"claude", "pi"} and os.path.lexists(published):
                    assert_maintenance_safe(selected, package=published)
        except BaseException:
            # Nothing was activated; leave no half-staged copies behind.
            for _selected, package, _published in staged:
                shutil.rmtree(package, ignore_errors=True)
            raise
        for selected, package, published in staged:
            if os.path.lexists(published):
                # Retain previous packages; they may contain qualification artifacts.
                published.rename(build_root / (published.name + "-previous." + uuid.uuid4().hex))
            package.rename(published)
            print(f"Built native {selected} plugin at {published}")


if __name__ == "__main__":
    try:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("--prebuilt", type=Path, help="Stage an existing runtime without Rust")
        parser.add_argument("--host", choices=[*HOST_PACKAGE_FILES, "all"], default="codex")
        parser.add_argument("--claude", default="claude", help="Claude Code CLI for native payload validation")
        args = parser.parse_args()
        build(SOURCE, args.prebuilt, args.host, args.claude)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
