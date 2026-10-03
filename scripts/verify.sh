#!/bin/bash
set -euo pipefail
SOURCE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$SOURCE"
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -B scripts/test_installation.py
python3 -B scripts/test_release.py
python3 -B scripts/test_verify_fresh_install.py
npm --prefix packages/installer test
