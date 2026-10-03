"""Build the DeLM runtime and stage a native Codex plugin. Never build Codex."""

import os
import argparse
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import uuid

from install_support import fingerprint, locked


SOURCE = Path(__file__).resolve().parent.parent
PACKAGE_FILES = (
    ".codex-plugin/plugin.json", "skills/run/SKILL.md",
    "skills/run/agents/openai.yaml", "hooks/hooks.json", "LICENSE", "NOTICE",
)


def stage_package(source, runtime, package):
    """Stage only runtime resources, never a checkout or its local artifacts."""
    fingerprint(runtime)
    (package / "bin").mkdir(parents=True)
    shutil.copy2(runtime, package / "bin/delm")
    (package / "bin/delm").chmod(0o755)
    for name in PACKAGE_FILES:
        original = source / name
        for component in original.relative_to(source).parents:
            if (source / component).is_symlink():
                raise RuntimeError(f"Refusing a linked package directory: {source / component}")
        fingerprint(original)
        destination = package / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(original, destination)


def build(source, prebuilt=None):
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
        package = Path(tempfile.mkdtemp(prefix="plugin-stage.", dir=build_root))
        stage_package(source, runtime, package)
        subprocess.run([str(package / "bin/delm"), "--version"], check=True)
        published = build_root / "plugin"
        if os.path.lexists(published):
            if not published.is_dir() or published.is_symlink():
                raise RuntimeError(f"Refusing to replace an unrelated path: {published}")
            # Retain previous packages; they may contain local qualification artifacts.
            published.rename(build_root / ("plugin-previous." + uuid.uuid4().hex))
        package.rename(published)
    print(f"Built native Codex plugin at {published}")


if __name__ == "__main__":
    try:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("--prebuilt", type=Path, help="Stage an existing runtime without Rust")
        args = parser.parse_args()
        build(SOURCE, args.prebuilt)
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
