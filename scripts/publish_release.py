"""Publish an already signed release from GitHub's protected release job only."""

import json
import os
from pathlib import Path
import subprocess
import sys

from package_release import verify
from release_identity import main as verify_identity


def git(*args, **kwargs):
    return subprocess.check_output(["git", *args], text=True, **kwargs).strip()


def main():
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise SystemExit("Publication is restricted to the release workflow.")
    verify_identity()
    package = Path(sys.argv[1]).resolve()
    verify(package)
    metadata = json.loads((package / "release.json").read_text())
    if not metadata["signedAndNotarized"]:
        raise SystemExit("Unsigned review packages cannot be published.")
    if metadata["sourceRevision"] != git("rev-parse", "HEAD"):
        raise SystemExit("Release provenance does not match the checked-out source.")
    tag = "delm-plugin-v" + metadata["version"]
    if git("ls-remote", "--tags", "origin", "refs/tags/" + tag):
        raise SystemExit(f"Immutable package tag already exists: {tag}")
    remote_branch = git("ls-remote", "--heads", "origin", "refs/heads/marketplace")
    parent = None
    if remote_branch:
        git("fetch", "origin", "refs/heads/marketplace")
        parent = git("rev-parse", "FETCH_HEAD")
    # Build the distribution tree with a separate index. The source checkout,
    # its index, and source branch remain untouched.
    index = Path(os.environ["RUNNER_TEMP"]) / "delm-release-index"
    if index.exists():
        raise SystemExit("Release index already exists; refusing to reuse it.")
    env = dict(os.environ, GIT_INDEX_FILE=str(index),
               GIT_AUTHOR_NAME="DeLM release", GIT_AUTHOR_EMAIL="release@users.noreply.github.com",
               GIT_COMMITTER_NAME="DeLM release", GIT_COMMITTER_EMAIL="release@users.noreply.github.com")
    git("read-tree", "--empty", env=env)
    git("--work-tree=" + str(package), "add", "--all", ".", env=env)
    tree = git("write-tree", env=env)
    args = ["commit-tree", tree, "-m", f"DeLM {metadata['version']} from {metadata['sourceRevision']}"]
    if parent:
        args.extend(["-p", parent])
    commit = git(*args, env=env)
    # Without force, concurrent or unrelated branch changes fail safely.
    git("push", "--atomic", "origin", commit + ":refs/heads/marketplace",
        commit + ":refs/tags/" + tag)


if __name__ == "__main__":
    main()
