"""Pin clean source for review; require a matching immutable tag for publication."""

import os
import re
import subprocess

from build import SOURCE
from package_release import source_state, source_version


def main():
    expected = "v" + source_version(SOURCE)
    source_ref = os.environ.get("SOURCE_REF", os.environ.get("SOURCE_TAG", ""))
    publishing = os.environ.get("RELEASE_PUBLISH", "true").lower() == "true"
    if not source_ref or source_ref.startswith("-"):
        raise SystemExit("Select a source branch, tag, or full commit SHA.")
    if publishing and source_ref not in [expected, "refs/tags/" + expected]:
        raise SystemExit(f"Publication requires the source tag {expected}; unsigned review accepts a branch or commit.")
    pinned = os.environ.get("RELEASE_SOURCE_SHA")
    if pinned is not None:
        if not re.fullmatch(r"[0-9a-f]{40}", pinned):
            raise SystemExit("Invalid pinned source revision.")
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    if pinned is None:
        selected = subprocess.check_output(["git", "rev-parse", "--verify", source_ref + "^{commit}"], text=True).strip()
        if selected != head:
            raise SystemExit("The selected source reference does not identify this checkout.")
    if pinned is not None and pinned != head:
        raise SystemExit("The pinned source revision does not identify this checkout.")
    if publishing:
        tagged = subprocess.check_output(["git", "rev-parse", "refs/tags/" + expected + "^{commit}"], text=True).strip()
        if tagged != head:
            raise SystemExit("The selected source tag does not identify this checkout.")
    if source_state(SOURCE)["sourceDirty"]:
        raise SystemExit("Release workflow qualification requires clean committed source; use local working-tree checks for drafts.")
    return head


if __name__ == "__main__":
    main()
