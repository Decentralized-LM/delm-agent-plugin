"""Assemble a prebuilt native marketplace tree; never install, sign, or publish it."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys

from build import SOURCE, stage_package
from install_support import package_files


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n")


def source_version(source):
    package = re.search(r"(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)",
                        (source / "Cargo.toml").read_text())
    match = re.search(r'^version\s*=\s*"([^"]+)"\s*$', package[1] if package else "", re.M)
    if not match:
        raise RuntimeError("Cargo.toml must contain an explicit package version.")
    version = match[1]
    manifest = ".codex-plugin/plugin.json"
    data = json.loads((source / manifest).read_text())
    if data.get("name") != "delm" or data.get("version") != version:
        raise RuntimeError(f"{manifest} must identify delm {version}.")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise RuntimeError("Invalid release version.")
    return version


def assemble(source, runtime, output, repository, revision, signed=False):
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise RuntimeError("Repository must be the GitHub OWNER/REPO, without a URL.")
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise RuntimeError("Source revision must be a full Git commit SHA.")
    if os.path.lexists(output):
        raise RuntimeError(f"Output must be a new directory; preserving {output}.")
    version = source_version(source)
    result = subprocess.run([str(runtime.resolve()), "--version"], check=True,
                            text=True, capture_output=True)
    if result.stdout.strip() != f"delm {version}":
        raise RuntimeError("Prebuilt runtime version differs from source manifests.")
    # Release assemblies must contain both native Mac architectures.
    subprocess.run(["lipo", "-verify_arch", "arm64", "x86_64", str(runtime)], check=True)
    package = output / "plugins/delm"
    stage_package(source, runtime, package)
    manifest = json.loads((package / ".codex-plugin/plugin.json").read_text())
    manifest["repository"] = f"https://github.com/{repository}"
    write_json(package / ".codex-plugin/plugin.json", manifest)
    write_json(output / ".agents/plugins/marketplace.json", {
        "name": "delm", "interface": {"displayName": "DeLM"},
        "plugins": [{"name": "delm", "source": {
            "source": "git-subdir", "url": f"https://github.com/{repository}.git",
            "path": "./plugins/delm", "ref": f"delm-plugin-v{version}"},
            "policy": {"installation": "AVAILABLE", "authentication": "ON_USE"},
            "category": "Developer Tools"}],
    })
    write_json(output / "release.json", {
        "schema": 1, "version": version, "sourceRevision": revision,
        "platform": "darwin", "architectures": ["arm64", "x86_64"],
        "minimumMacOS": "13.0", "signedAndNotarized": signed,
        "files": package_files(package),
    })
    (output / "README.md").write_text(
        f"# DeLM {version} for macOS\n\n"
        "Prebuilt native Codex plugin for Apple Silicon and Intel. "
        "Requires stock Codex CLI and Git; no Rust or Python is needed.\n\n"
        + ("Signed and notarized release.\n\n" if signed else
           "**Unsigned review artifact. Not for public distribution or installation.**\n\n")
        + "Once this signed release is published, install with:\n\n```sh\n"
        f"codex plugin marketplace add {repository} --ref marketplace && codex plugin add delm@delm\n"
        "```\n\nRestart Codex, open `/hooks`, and review and trust the DeLM hooks. "
        "Restart Codex once more, then invoke `$delm:run <task>`. Installation starts no workers and does not grant hook trust.\n\n"
        "Update with `codex plugin marketplace upgrade delm`, review any changed hooks in `/hooks`, and restart before running DeLM. "
        "Remove with `codex plugin remove delm@delm`; saved DeLM runs are retained.\n\n"
        f"[Source and support](https://github.com/{repository}) · "
        "[Project](https://yuzhenmao.github.io/DeLM/) · "
        "[Paper](https://arxiv.org/abs/2606.10662)\n")
    # List the complete distribution, including its catalog and provenance.
    entries = package_files(output)
    (output / "SHA256SUMS").write_text("".join(
        f"{info['sha256']}  {name}\n" for name, info in entries.items()))
    return output


def verify(output):
    metadata = json.loads((output / "release.json").read_text())
    if package_files(output / "plugins/delm") != metadata["files"]:
        raise RuntimeError("Package files, permissions, or checksums changed.")
    expected = {}
    for line in (output / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        if name in expected or Path(name).is_absolute() or ".." in Path(name).parts:
            raise RuntimeError("Invalid checksum file.")
        expected[name] = digest
    actual = {name: info["sha256"] for name, info in package_files(output).items()
              if name != "SHA256SUMS"}
    if actual != expected:
        raise RuntimeError("Distribution files or checksums changed.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", type=Path)
    parser.add_argument("--runtime", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--repository")
    parser.add_argument("--revision")
    parser.add_argument("--signed-and-notarized", action="store_true")
    args = parser.parse_args()
    if args.verify:
        verify(args.verify)
    else:
        if not all((args.runtime, args.output, args.repository, args.revision)):
            parser.error("--runtime, --output, --repository, and --revision are required")
        if args.signed_and_notarized:
            subprocess.run(["codesign", "--verify", "--strict", "--check-notarization",
                            "-R=notarized", str(args.runtime)], check=True)
        assemble(SOURCE, args.runtime, args.output, args.repository, args.revision,
                 args.signed_and_notarized)
        verify(args.output)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
