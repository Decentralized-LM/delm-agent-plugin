"""Require a source tag that agrees with the runtime and native plugin manifest."""

import os
import re
import subprocess

from build import SOURCE
from package_release import source_version


def main():
    expected = "v" + source_version(SOURCE)
    if os.environ.get("SOURCE_TAG") != expected:
        raise SystemExit(f"Select the source tag {expected}.")
    tag = os.environ.get("RELEASE_SOURCE_SHA")
    if tag is not None:
        if not re.fullmatch(r"[0-9a-f]{40}", tag):
            raise SystemExit("Invalid pinned source revision.")
    else:
        tag = subprocess.check_output(["git", "rev-parse", "refs/tags/" + expected + "^{commit}"], text=True).strip()
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    if tag != head:
        raise SystemExit("The selected tag does not identify this checkout.")


if __name__ == "__main__":
    main()
