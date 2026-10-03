"""Publish an already signed release from GitHub's protected release job only."""

import os
from pathlib import Path
import subprocess
import sys

from package_release import verify
from qualify_release import verify_signed
from release_identity import main as verify_identity


def git(*args, **kwargs):
    return subprocess.check_output(["git", *args], text=True, **kwargs).strip()


def verify_destination(repository):
    """Both read and write remotes must identify the catalog's repository."""
    expected = repository.lower()
    accepted = {f"https://github.com/{expected}", f"https://github.com/{expected}.git",
                f"git@github.com:{expected}", f"git@github.com:{expected}.git",
                f"ssh://git@github.com/{expected}", f"ssh://git@github.com/{expected}.git"}
    for flags in [("--all",), ("--push", "--all")]:
        urls = git("remote", "get-url", *flags, "origin").splitlines()
        if len(urls) != 1 or urls[0].lower() not in accepted:
            raise SystemExit("The origin remote does not match the release repository; refusing to publish to a different destination.")


def main():
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise SystemExit("Publication is restricted to the release workflow.")
    if os.environ.get("RELEASE_PUBLISH", "true").lower() != "true":
        raise SystemExit("Unsigned review cannot enter the publication step.")
    revision = verify_identity()
    repository = os.environ.get("RELEASE_REPOSITORY")
    if not repository or not os.environ.get("RELEASE_SOURCE_SHA"):
        raise SystemExit("Publication requires an explicit repository and pinned source revision.")
    if len(sys.argv) != 3:
        raise SystemExit("Usage: publish_release.py PACKAGE SIGNED_QUALIFICATION_REPORTS")
    package = Path(sys.argv[1]).resolve()
    metadata = verify(package, revision=revision, repository=repository, require_qualified=True)
    if not metadata["signedAndNotarized"]:
        raise SystemExit("Unsigned review packages cannot be published.")
    verify_signed(package, Path(sys.argv[2]).resolve())
    verify_destination(repository)
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
