"""Run current release tooling against the unchanged, tagged source checkout."""

from pathlib import Path
import runpy
import sys

import build


def main():
    allowed = {"release_identity.py", "package_release.py", "qualify_release.py", "publish_release.py", "prepare_installer.py"}
    if len(sys.argv) < 2 or sys.argv[1] not in allowed:
        raise SystemExit("Select a supported release script.")
    # Workflows check out the candidate at cwd and current tooling separately.
    # Keep source and qualification hashes bound to the candidate, not the tooling.
    build.SOURCE = Path.cwd().resolve()
    script = Path(__file__).resolve().parent / sys.argv[1]
    sys.argv = [str(script), *sys.argv[2:]]
    runpy.run_path(str(script), run_name="__main__")


if __name__ == "__main__":
    main()
