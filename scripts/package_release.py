"""Assemble a prebuilt native marketplace tree; never install, sign, or publish it."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

from build import PACKAGE_FILES, SOURCE, stage_package
from install_support import fingerprint, package_files


ARCHITECTURES = {"arm64": "aarch64-apple-darwin", "x86_64": "x86_64-apple-darwin"}
LIFECYCLE_CASES = ["interrupt", "preflight", "stop", "owner-death", "plugin-remove"]
REPOSITORY_PATTERN = r"[A-Za-z0-9][A-Za-z0-9_.-]*/[A-Za-z0-9][A-Za-z0-9_.-]*"


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
    lock = (source / "Cargo.lock").read_text()
    if not re.search(r'(?m)^name = "delm"\nversion = "' + re.escape(version) + r'"$', lock):
        raise RuntimeError("Cargo.lock must match the DeLM package version.")
    return version


def source_state(source):
    """Bind build inputs and distinguish a working tree from committed source."""
    inputs = {name: fingerprint(source / name)["sha256"]
              for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]}
    for directory in ["src", "plugin"]:
        inputs.update({f"{directory}/{name}": entry["sha256"]
                       for name, entry in package_files(source / directory).items()})
    digest = hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()
    status = subprocess.run(["git", "-C", str(source), "status", "--porcelain", "--untracked-files=no"],
                            text=True, capture_output=True)
    untracked = subprocess.run(["git", "-C", str(source), "ls-files", "--others", "--exclude-standard",
                                "--", "src", "plugin", "scripts", "tests", "skills", "hooks", "packages", ".github"],
                               text=True, capture_output=True)
    dirty = bool(status.returncode or untracked.returncode or status.stdout or untracked.stdout)
    return {"sourceDirty": dirty, "runtimeSourcesSha256": digest}


def verify_qualification(metadata, runtime=None):
    records = metadata.get("qualification", {})
    if set(records) != set(ARCHITECTURES):
        raise RuntimeError("Both native macOS architectures must pass release qualification.")
    for arch, record in records.items():
        if (record.get("schema") != 1 or record.get("kind") != "native-release-build"
                or record.get("architecture") != arch or record.get("target") != ARCHITECTURES[arch]
                or record.get("sourceRevision") != metadata["sourceRevision"]
                or record.get("runtimeSourcesSha256") != metadata["runtimeSourcesSha256"]
                or record.get("sourceDirty") is not False or record.get("passed") is not True
                or record.get("modelCalls") != 0
                or record.get("lifecycleCases") != LIFECYCLE_CASES
                or not re.fullmatch(r"[0-9a-f]{64}", record.get("runtimeSha256", ""))):
            raise RuntimeError(f"Invalid native release qualification for {arch}.")
        if runtime is not None:
            with tempfile.TemporaryDirectory(prefix="delm-slice-") as directory:
                sliced = Path(directory) / "delm"
                subprocess.run(["lipo", str(runtime), "-thin", arch, "-output", str(sliced)], check=True)
                if fingerprint(sliced)["sha256"] != record["runtimeSha256"]:
                    raise RuntimeError(f"The {arch} release slice differs from the tested binary.")


def assemble(source, runtime, output, repository, revision, signed=False,
             qualifications=(), unsigned_origin=None):
    if not re.fullmatch(REPOSITORY_PATTERN, repository):
        raise RuntimeError("Repository must be the GitHub OWNER/REPO, without a URL.")
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise RuntimeError("Source revision must be a full Git commit SHA.")
    if os.path.lexists(output):
        raise RuntimeError(f"Output must be a new directory; preserving {output}.")
    version = source_version(source)
    provenance = source_state(source)
    qualification = {}
    if unsigned_origin is not None:
        origin = verify(unsigned_origin, revision=revision, repository=repository, require_qualified=True)
        if origin["signedAndNotarized"] or origin["runtimeSourcesSha256"] != provenance["runtimeSourcesSha256"]:
            raise RuntimeError("Signing source differs from the qualified unsigned package.")
        verify_qualification(origin, unsigned_origin / "plugins/delm/bin/delm")
        qualification = origin["qualification"]
    for path in qualifications:
        record = json.loads(path.read_text())
        architecture = record.get("architecture")
        if architecture in qualification:
            raise RuntimeError("Duplicate architecture qualification.")
        qualification[architecture] = record
    if signed and (unsigned_origin is None or provenance["sourceDirty"]):
        raise RuntimeError("Signed releases require clean source and its qualified unsigned package.")
    result = subprocess.run([str(runtime.resolve()), "--version"], check=True,
                            text=True, capture_output=True)
    if result.stdout.strip() != f"delm {version}":
        raise RuntimeError("Prebuilt runtime version differs from source manifests.")
    # Release assemblies must contain both native Mac architectures.
    subprocess.run(["lipo", str(runtime), "-verify_arch", "arm64", "x86_64"], check=True)
    package = output / "plugins/delm"
    stage_package(source, runtime, package)
    manifest = json.loads((package / ".codex-plugin/plugin.json").read_text())
    manifest["repository"] = f"https://github.com/{repository}"
    write_json(package / ".codex-plugin/plugin.json", manifest)
    if unsigned_origin is not None:
        payload = {name: value for name, value in package_files(package).items() if name != "bin/delm"}
        original_payload = {name: value for name, value in origin["files"].items() if name != "bin/delm"}
        if payload != original_payload:
            raise RuntimeError("Signed plugin payload differs from the qualified unsigned package.")
    write_json(output / ".agents/plugins/marketplace.json", {
        "name": "delm", "interface": {"displayName": "DeLM"},
        "plugins": [{"name": "delm", "source": {
            "source": "git-subdir", "url": f"https://github.com/{repository}.git",
            "path": "./plugins/delm", "ref": f"delm-plugin-v{version}"},
            "policy": {"installation": "AVAILABLE", "authentication": "ON_USE"},
            "category": "Developer Tools"}],
    })
    metadata = {
        "schema": 1, "version": version, "sourceRevision": revision,
        "repository": repository, **provenance,
        "platform": "darwin", "architectures": ["arm64", "x86_64"],
        "minimumMacOS": "13.0", "signedAndNotarized": signed,
        "files": package_files(package),
        "qualification": qualification,
    }
    if qualification:
        verify_qualification(metadata, None if signed else runtime)
    if signed:
        metadata["unsignedRuntimeSha256"] = origin["files"]["bin/delm"]["sha256"]
    write_json(output / "release.json", metadata)
    (output / "README.md").write_text(
        f"# DeLM {version} for macOS\n\n"
        "Prebuilt native Codex plugin for Apple Silicon and Intel. "
        "Requires stock Codex CLI and Git; no Rust or Python is needed.\n\n"
        + ("Signed and notarized release.\n\n" if signed else
           "**Unsigned review artifact. Not for public distribution or installation.**\n\n")
        + ("Built from an uncommitted working tree. `sourceRevision` identifies its base commit, "
           "not the exact source; `runtimeSourcesSha256` records the runtime source inputs.\n\n"
           if provenance["sourceDirty"] else "")
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


def verify(output, revision=None, repository=None, require_qualified=False):
    metadata = json.loads((output / "release.json").read_text())
    if (metadata.get("schema") != 1 or metadata.get("platform") != "darwin"
            or metadata.get("architectures") != list(ARCHITECTURES)
            or metadata.get("minimumMacOS") != "13.0"
            or type(metadata.get("signedAndNotarized")) is not bool
            or type(metadata.get("sourceDirty")) is not bool
            or not re.fullmatch(r"[0-9a-f]{40}", metadata.get("sourceRevision", ""))
            or not re.fullmatch(r"[0-9a-f]{64}", metadata.get("runtimeSourcesSha256", ""))
            or not re.fullmatch(REPOSITORY_PATTERN, metadata.get("repository", ""))):
        raise RuntimeError("Invalid release provenance or platform metadata.")
    if revision is not None and metadata["sourceRevision"] != revision:
        raise RuntimeError("Release source revision differs from the pinned checkout.")
    if repository is not None and metadata["repository"] != repository:
        raise RuntimeError("Release repository differs from the expected destination.")
    version = metadata.get("version", "")
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", version):
        raise RuntimeError("Invalid release version.")
    package = output / "plugins/delm"
    manifest = json.loads((package / ".codex-plugin/plugin.json").read_text())
    if (manifest.get("name") != "delm" or manifest.get("version") != version
            or manifest.get("repository") != "https://github.com/" + metadata["repository"]
            or (package / "plugin.json").exists()):
        raise RuntimeError("Native plugin manifest differs from release provenance.")
    catalog = json.loads((output / ".agents/plugins/marketplace.json").read_text())
    expected_source = {"source": "git-subdir", "url": "https://github.com/" + metadata["repository"] + ".git",
                       "path": "./plugins/delm", "ref": "delm-plugin-v" + version}
    plugins = catalog.get("plugins", [])
    if (catalog.get("name") != "delm" or len(plugins) != 1 or plugins[0].get("name") != "delm"
            or plugins[0].get("source") != expected_source):
        raise RuntimeError("Marketplace catalog differs from the immutable release identity.")
    plugin_files = package_files(package)
    if plugin_files != metadata["files"]:
        raise RuntimeError("Package files, permissions, or checksums changed.")
    allowed_plugin_files = {*PACKAGE_FILES, "bin/delm"}
    if set(plugin_files) != allowed_plugin_files:
        raise RuntimeError("Release plugin contents differ from the runtime resource allowlist.")
    if metadata["files"].get("bin/delm", {}).get("mode") != 0o755:
        raise RuntimeError("Release runtime must be executable with mode 0755.")
    expected = {}
    for line in (output / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        if (name in expected or Path(name).is_absolute() or ".." in Path(name).parts
                or not re.fullmatch(r"[0-9a-f]{64}", digest)):
            raise RuntimeError("Invalid checksum file.")
        expected[name] = digest
    distribution_files = package_files(output)
    allowed_distribution_files = {"README.md", "release.json", "SHA256SUMS",
                                  ".agents/plugins/marketplace.json"}
    allowed_distribution_files.update("plugins/delm/" + name for name in allowed_plugin_files)
    if set(distribution_files) != allowed_distribution_files:
        raise RuntimeError("Release distribution contains missing or unexpected artifacts.")
    actual = {name: info["sha256"] for name, info in distribution_files.items()
              if name != "SHA256SUMS"}
    if actual != expected:
        raise RuntimeError("Distribution files or checksums changed.")
    if require_qualified or metadata["signedAndNotarized"]:
        if metadata["sourceDirty"]:
            raise RuntimeError("An uncommitted working-tree artifact cannot qualify for publication.")
        verify_qualification(metadata)
    if metadata["signedAndNotarized"] and not re.fullmatch(
            r"[0-9a-f]{64}", metadata.get("unsignedRuntimeSha256", "")):
        raise RuntimeError("Signed release must identify its qualified unsigned runtime.")
    return metadata


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify", type=Path)
    parser.add_argument("--runtime", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--repository")
    parser.add_argument("--revision")
    parser.add_argument("--signed-and-notarized", action="store_true")
    parser.add_argument("--qualification", action="append", default=[], type=Path)
    parser.add_argument("--unsigned-origin", type=Path)
    parser.add_argument("--require-qualified", action="store_true")
    args = parser.parse_args()
    if args.verify:
        verify(args.verify, revision=args.revision, repository=args.repository,
               require_qualified=args.require_qualified)
    else:
        if not all((args.runtime, args.output, args.repository, args.revision)):
            parser.error("--runtime, --output, --repository, and --revision are required")
        if args.signed_and_notarized:
            subprocess.run(["codesign", "--verify", "--strict", "--check-notarization",
                            "-R=notarized", str(args.runtime)], check=True)
        assemble(SOURCE, args.runtime, args.output, args.repository, args.revision,
                 args.signed_and_notarized, args.qualification, args.unsigned_origin)
        verify(args.output)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
