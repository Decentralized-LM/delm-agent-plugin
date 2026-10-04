# Development

Use macOS with stock Codex CLI on `PATH`, an existing Codex login, Git, Python 3, Xcode Command Line Tools, and the Rust toolchain pinned in `rust-toolchain.toml`. Installer tests also need Node.js 22 or later.

Build and install through Codex's native plugin manager:

```sh
./scripts/install.sh
```

Restart Codex, review and trust DeLM in `/hooks`, then restart again. Open the intended project and invoke `$delm:run <task>`. The contributor marketplace is `delm-local`. Installation neither replaces Codex nor grants hook trust.

To build without installing, run `./scripts/build.sh`. It stages a package under `.build/plugin/` and preserves earlier packages. A compatible prebuilt runtime can be supplied with `--prebuilt /absolute/path/to/delm`.

## Focused verification

```sh
./scripts/verify.sh
npm --prefix packages/installer run test:native
DELM_TEST_HOST="$(command -v codex)" cargo test --locked --test native_inheritance -- --ignored
node --test examples/task-board/task.test.mjs
```

The regular suite uses deterministic workers. It covers task ownership, publication/import checks, shared verification, service ownership, guarded original-project delivery, recovery, and cleanup. Installer tests use temporary Codex homes and local repositories.

Native inheritance qualification creates metadata-only parent and forked sessions in disposable storage, compares a project skill and local documentation MCP, calls that local tool, and verifies the returned native permission settings. It starts no model turn and opens no browser. This establishes the tested saved-configuration path, not exact parity with every override and live connection in an existing user session.

The [native lifecycle fixtures](native-lifecycle-qualification.md) cover interruption, startup cancellation, parent completion, owner-process death, and plugin removal. Their scripted provider does not call a real model. Run native checks from a terminal that permits local sockets and nested native processes.

For changes affecting launch or delivery, measure invocation-to-worker and completion-to-project separately. Do not omit parent handoff, environment setup, or conflict resolution from user-visible task latency. Use focused fixtures during development; a long application build is not a prerequisite for every edit.

## Installer and release artifacts

The [npm installer](../packages/installer/README.md) has no dependencies or installation scripts. Its version is independent of the plugin version. Source configuration leaves the destination unset; release preparation supplies the selected repository:

```sh
python3 scripts/prepare_installer.py --repository OWNER/REPOSITORY --out .validation/prepared-installer
```

Use a new output directory. The generated package includes its tarball, checksums, and preparation record. Its native installation test uses disposable homes and a local marketplace; it does not register a plugin in your normal Codex configuration.

Release architecture, signing, and publication requirements are documented in [releases](releases.md). Cross-compilation does not establish native Intel qualification. Local checks do not establish that a release workflow, signing, or notarization has passed.

## Optional real-task qualification

Only run this when real account use has been authorized. Supply a small task beginning with `$delm:run `:

```sh
python3 scripts/verify_fresh_install.py \
  --out .validation/fresh-install-01 \
  --auth-home "${CODEX_HOME:-$HOME/.codex}" \
  --runtime target/debug/delm \
  --model gpt-6-astra --effort xhigh \
  --task-file /absolute/path/to/task.txt \
  --timeout-seconds 1500
```

Select an available model and effort. This helper installs the plugin into a disposable Codex home, trusts its discovered hooks there, and invokes a real parent session. It uses real account capacity and stays outside routine CI. It references the existing file-backed login without copying its contents and removes that reference after confirmed shutdown.

Evidence covers native ownership, exact task handoff, both workers, observed collaboration, delivery to the disposable original project, and removal of temporary workspaces. A `delivered` status is distinct from fully verified completion: required local setup or merged-result checks must finish in the parent before reporting the task ready. This qualifies a fresh installation, not new-account login or subjective product quality. Keep its evidence private.

Repository preparation can also be measured without any model work:

```sh
cargo test --locked --lib preparation_timing_with_many_ignored_directories -- --ignored --nocapture
```

The fixture measures the same saved project three times. Compare like machines and build profiles; preparation time is not complete task latency.

## Contributor hygiene

Use the user's normal installed browser tools and native permissions. Do not prescribe a special Chromium sandbox workaround or disable their integrations to make a fixture pass. When browser state is shared, coordinate the relevant resource or use independent browser contexts.

Remove a contributor installation with `./scripts/uninstall.sh`. Follow [migration](support.md#updating-and-removing-the-plugin) before switching a modified local installation to a published package. Make edits in this source checkout, not the installed cache.

Keep credentials, private run folders, local builds, and research evidence out of release packages. Packaging uses an explicit allowlist and records checksums. No routine qualification command commits or publishes changes.
