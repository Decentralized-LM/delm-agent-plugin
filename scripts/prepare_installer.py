"""Prepare a configured npm installer; never publish or contact its destination."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


SOURCE = Path(__file__).resolve().parents[1] / "packages/installer"
PACKAGE_FILES = ["LICENSE", "NOTICE", "README.md", "bin/delm-agent.mjs",
                 "lib/claude.mjs", "lib/hosts.mjs", "lib/installer.mjs", "lib/maintenance.mjs", "lib/native.mjs", "package.json", "release.json"]
REPOSITORY_PATTERN = r"[A-Za-z0-9][A-Za-z0-9_.-]*/[A-Za-z0-9][A-Za-z0-9_.-]*"


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def readme(repository):
    return f"""# DeLM installer

Install and manage native DeLM plugins for Codex or Claude Code. Both use the
`marketplace` branch of [the DeLM distribution](https://github.com/{repository})
and the identity `delm@delm`. One command detects your installed host and offers
a choice when both Codex and Claude Code are available.

## Install

Requirements: macOS, Node.js 22+, Git, and the selected host CLI on `PATH`.
Claude Code requires version 2.1.289 or newer, our qualified support floor for
the native fork/Mods API contract. Install and update check this before changing
anything; status and removal remain available with compatible older native JSON.
Complete account login through your host before running DeLM. No Rust compiler
or source checkout is needed.

```sh
npx --yes delm-agent@latest install
```

If one host CLI is available, it is selected automatically. If both are available,
choose **Codex**, **Claude Code**, or **Both**. Cancelling the selection makes no
changes. If neither is available, install a host CLI first.

For Codex, restart, open `/hooks`, and review and trust DeLM when requested.
Restart after granting trust, then enter `$delm:run <task>`. A desktop or IDE
installation alone is insufficient without stock Codex CLI.

For Claude Code, restart to load the plugin, then enter `/delm:run <task>`.
The installer manages Claude's user scope and respects `CLAUDE_CONFIG_DIR`.
It does not grant tool permissions, inspect credentials, or start workers.

## Manage

```sh
npx --yes delm-agent@latest status
npx --yes delm-agent@latest update
npx --yes delm-agent@latest remove
```

Stop active DeLM work before updating or removing the plugin. Repeating `install`
leaves an enabled plugin in place. Updating a disabled plugin keeps it disabled;
`install` enables it. Installation status does not verify activation or permissions
in an existing session. Removal retains saved work and the marketplace; Claude
removal uses its native `--keep-data` option. Native removal may clear that plugin's
stored options. Unrelated plugins and host account credentials are preserved.
Installer and plugin versions are independent; plugin updates use the host's manager.

Before native mutations, the installer checks local DeLM run records for the
selected host. Active runs, uncertain state, or remaining worker directories
block changes with recovery guidance. Keep DeLM stopped throughout maintenance;
the preflight does not lock other host sessions. Read-only status stays available.

Structured status includes `readiness.installation`, `readiness.session` (reported
as `not_checked`), and host-specific next steps. An enabled installation does not
establish that a running session has loaded it or accepted its trust requirements.

## Host selection

All commands use the same host selection. Scripts can skip the menu:

```sh
npx --yes delm-agent@latest install --host codex
npx --yes delm-agent@latest install --host claude
npx --yes delm-agent@latest install --host both
```

If both CLIs are available, noninteractive commands and `--json` need an explicit
selection. npm's `--yes` skips its download confirmation, not this host choice.

Use `--codex PATH` or `--claude PATH` to select an existing CLI. A single executable
option implies that host; supplying both implies both hosts. Options conflicting
with an explicit single host are rejected. Use `--json` for structured output.
For one host, output includes `host`; for both, output includes `command`,
`success`, `results`, and per-host `errors`. Both-host operations run in sequence.
One failure does not undo the other host's success; any failure makes the overall
command exit with an error. Retry the failed host using its explicit flag.

`CODEX_HOME` and `CLAUDE_CONFIG_DIR` are respected.
`--help`, `--version`, and read-only `status` are also available on other operating
systems. Git access to the distribution is required; a private repository requires
your own authorized Git access.

Conflicting source registrations are preserved. Claude registrations on another
branch or in another plugin scope block changes. An existing `delm@delm-local`
source installation blocks install and update. For Codex, stop active work, run
`./scripts/uninstall.sh` from its original checkout, and follow that checkout's
migration instructions if cached files were modified. For Claude, review the
source installation in its native plugin manager. This installer does not remove
or migrate source installations automatically.
"""


def prepare(repository, output, native_release=None, source=SOURCE):
    if not re.fullmatch(REPOSITORY_PATTERN, repository):
        raise RuntimeError("Repository must be a GitHub OWNER/REPO name, not a URL or path.")
    source = Path(source).resolve()
    output = Path(output).absolute()
    if output.exists() or output.is_symlink():
        raise RuntimeError("Installer output already exists; choose a new directory.")
    if output.resolve().is_relative_to(source):
        raise RuntimeError("Installer output must be outside the source package.")
    metadata = json.loads((source / "package.json").read_text())
    configuration = json.loads((source / "release.json").read_text())
    if metadata.get("private") is not True or configuration != {
        "repository": None, "marketplace": "delm", "ref": "marketplace", "plugin": "delm@delm"
    }:
        raise RuntimeError("Preparation requires the private, unconfigured source installer.")
    if metadata.get("name") != "delm-agent" or not re.fullmatch(
            r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", metadata.get("version", "")):
        raise RuntimeError("Source must identify delm-agent with an explicit installer version.")
    native = None
    if native_release is not None:
        native_path = Path(native_release)
        native_bytes = native_path.read_bytes()
        record = json.loads(native_bytes)
        if (record.get("schema") != 1 or record.get("repository") != repository
                or not isinstance(record.get("version"), str)
                or not re.fullmatch(r"[0-9a-f]{40}", record.get("sourceRevision", ""))
                or not re.fullmatch(r"[0-9a-f]{64}", record.get("runtimeSourcesSha256", ""))):
            raise RuntimeError("Native release metadata must identify the same repository and valid source provenance.")
        native = {key: record[key] for key in ["schema", "repository", "version", "sourceRevision", "runtimeSourcesSha256"]}
        native["sha256"] = hashlib.sha256(native_bytes).hexdigest()
    for relative in PACKAGE_FILES:
        path = source / relative
        if path.is_symlink() or not path.is_file():
            raise RuntimeError(f"Installer input must be a regular file: {relative}")
    # Refuse reuse atomically; a failed attempt remains available for inspection.
    output.mkdir(parents=True, exist_ok=False)
    package = output / "package"
    package.mkdir()
    for relative in PACKAGE_FILES:
        destination = package / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source / relative, destination)
    configuration["repository"] = repository
    write_json(package / "release.json", configuration)
    metadata["private"] = False
    metadata["repository"] = {"type": "git", "url": f"git+https://github.com/{repository}.git"}
    metadata.pop("scripts", None)
    write_json(package / "package.json", metadata)
    (package / "README.md").write_text(readme(repository), encoding="utf-8")
    # npm configuration and cache are disposable. No lifecycle scripts, login,
    # registry lookup, or publication is needed to create the tarball.
    with tempfile.TemporaryDirectory(prefix="delm-installer-npm-") as temporary:
        temporary = Path(temporary)
        for filename in ["user.npmrc", "global.npmrc"]:
            (temporary / filename).write_text("")
        env = {key: value for key, value in os.environ.items() if not key.lower().startswith("npm_config_")}
        env.update({"npm_config_userconfig": str(temporary / "user.npmrc"),
                    "npm_config_globalconfig": str(temporary / "global.npmrc"),
                    "npm_config_cache": str(temporary / "cache")})
        packed = json.loads(subprocess.check_output([
            "npm", "pack", "--json", "--ignore-scripts", "--offline", "--no-update-notifier",
            "--pack-destination", str(output),
        ], cwd=package, env=env, text=True))
    expected_tarball = f"delm-agent-{metadata['version']}.tgz"
    if (len(packed) != 1 or packed[0].get("filename") != expected_tarball
            or sorted(item["path"] for item in packed[0].get("files", [])) != PACKAGE_FILES):
        raise RuntimeError("npm packed unexpected installer contents; preserve the output for review.")
    tarball = output / expected_tarball
    preparation = {"schema": 1, "repository": repository,
                   "installer": {"name": metadata["name"], "version": metadata["version"]},
                   "tarball": {"path": expected_tarball, "sha256": sha256(tarball)},
                   "files": {relative: sha256(package / relative) for relative in PACKAGE_FILES}}
    if native is not None:
        preparation["nativeRelease"] = native
    write_json(output / "preparation.json", preparation)
    checksums = {expected_tarball: preparation["tarball"]["sha256"],
                 "preparation.json": sha256(output / "preparation.json")}
    (output / "SHA256SUMS").write_text("".join(f"{digest}  {name}\n" for name, digest in sorted(checksums.items())))
    return preparation


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--native-release", type=Path)
    args = parser.parse_args()
    try:
        result = prepare(args.repository, args.out, args.native_release)
    except (RuntimeError, OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Installer preparation failed: {error}\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
