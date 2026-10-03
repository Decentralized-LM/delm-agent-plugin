#!/bin/bash
set -euo pipefail
SOURCE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
exec python3 "$SOURCE/scripts/install_support.py" install "$@"
