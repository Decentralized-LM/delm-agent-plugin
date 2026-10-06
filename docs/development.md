# Development

Use macOS with Git, Python 3, Xcode Command Line Tools, the Rust toolchain pinned in `rust-toolchain.toml`, and Node.js 22 or later. Install the native CLI for the host you are developing: stock Codex, or Claude Code 2.1.289 or later. Account login is needed for interactive use and explicitly authorized real-task checks; routine verification does not start model turns.

Run the following commands from the repository root. Host packages share one Rust runtime and the collaboration policy in `plugin/worker.md`.

## Local host setup

### Codex plugin

Build and install through the native plugin manager:

```sh
./scripts/install.sh
```

Restart Codex, review and trust DeLM in `/hooks`, then restart again. Open the intended project using `codex --ask-for-approval on-request` and invoke `$delm:run <task>`. Confirm 2, 3, or 4 agents in the native form; 2 is selected initially. The contributor marketplace is `delm-local`. Installation neither replaces Codex nor grants hook trust.

To build without installing, run `./scripts/build.sh --host codex`. It stages the package under `.build/plugin/` and preserves earlier packages.

### Claude Code plugin

Build and install through the native plugin manager:

```sh
./scripts/build.sh --host claude
claude plugin marketplace add "$PWD" --scope user
claude plugin install delm@delm-local --scope user
```

The build stages `.build/plugin-claude/` and validates it with the official CLI. The repository's local marketplace points to that package. Restart Claude Code, open the intended project, and invoke `/delm:run <task>`. The live board opens in the same terminal; `/delm-status` reopens it after hiding. Review native trust or permission prompts. Claude resources live in `hosts/claude/`; the build copies the shared worker policy and both board modules into the package.

The native manager reads this local package in place. To load source changes, stop active DeLM work, rebuild, and restart Claude Code. Keep the checkout and staged package available while this source installation is registered. For a session without marketplace registration, open the intended project and use `claude --plugin-dir /absolute/path/to/this-checkout/.build/plugin-claude` instead.

### Shared builds

`./scripts/build.sh --host all` builds the runtime once and stages both packages. A compatible prebuilt runtime can be supplied with `--prebuilt /absolute/path/to/delm`. Neither staging operation installs a plugin or changes the user's native permissions.

## Focused verification

```sh
./scripts/verify.sh
```

This runs Rust formatting, linting, and tests; Python packaging and qualification-helper tests; installer tests; and Claude module tests. The deterministic suites cover task ownership, publication/import checks, shared verification, service ownership, guarded original-project delivery, recovery, and cleanup.

CI tests the qualified Codex and Claude versions on native Apple Silicon and Intel runners, and their latest versions together on Apple Silicon. Host upgrades must pass the same model-free checks. The latest-version job detects compatibility changes; it does not establish support before it passes.

Run the relevant native boundary checks when changing installation, inheritance, or lifecycle behavior:

```sh
npm --prefix packages/installer run test:native
npm --prefix packages/installer run test:native:claude
DELM_TEST_HOST="$(command -v codex)" cargo test --locked --test native_inheritance -- --ignored
node --test examples/task-board/task.test.mjs
```

The installer commands exercise Codex and Claude Code respectively, using disposable host configurations and local repositories. They do not use an account or start model turns. The task-board example is a separate, model-free fixture.

The Codex native inheritance check creates metadata-only parent and forked sessions in disposable storage, compares a project skill and local documentation MCP, calls that local tool, and verifies the returned native permission settings. It starts no model turn and opens no browser. This establishes the tested saved-configuration path, not exact parity with every override and live connection in an existing user session.

The [Codex native lifecycle fixtures](native-lifecycle-qualification.md) cover interruption, startup cancellation, parent completion, owner-process death, and plugin removal. Their scripted provider does not call a real model. Claude's module and controller tests cover its native event routing and lifecycle contract; the account-backed fixture below separately exercises real forks and collaboration. Run native checks from a terminal that permits local sockets and nested native processes.

Claude board changes also need passive-observer, renderer, and view-lifecycle checks. Follow the [board qualification checklist](terminal-ui-plan.md) for native layout, prompt focus, keyboard interaction, themes, hiding, and conversation changes. Package validation checks supported APIs and resources; it does not establish those interactive behaviors. Keep display refresh independent of the control stream and use passive view data for `/delm-status`.

Claude Code 2.1.289 accepts one entry in `hooks.json`'s `modules` list and permits static imports only; a second entry or `import()` prevents loading. Keep imported view modules free of fallible startup work and guard their initialization, observer, and rendering operations. Those guards isolate runtime view failures; malformed module syntax or a refused native API can still prevent the plugin from loading and must be caught by package validation.

For changes affecting launch or delivery, measure invocation-to-worker and completion-to-project separately. Do not omit parent handoff, environment setup, or conflict resolution from user-visible task latency. Use focused fixtures during development; a long application build is not a prerequisite for every edit.

## Installer and release artifacts

The [npm installer](../packages/installer/README.md) has no dependencies or installation scripts. Its version is independent of the plugin version. Source configuration leaves the destination unset; release preparation supplies the selected repository:

```sh
python3 scripts/prepare_installer.py --repository OWNER/REPOSITORY --out .validation/prepared-installer
```

Use a new output directory. The generated package includes its tarball, checksums, and preparation record. Native installer tests for both hosts use disposable homes and a local marketplace; they do not register plugins in your normal host configuration.

Release architecture, signing, and publication requirements are documented in [releases](releases.md). Cross-compilation does not establish native Intel qualification. Local checks do not establish that a release workflow, signing, or notarization has passed.

## Optional real-task qualification

### Codex

For the agent selector, use the bounded native-terminal fixture first:

```sh
cargo build --locked --bin delm
python3 scripts/verify_codex_selector.py --out .validation/selector-cancel --case cancel
```

This stages the production plugin in a disposable Codex home and presses Escape in the real native form. It makes no model calls. `--case never`, `--case normal`, and `--case worker` check policy refusal, ordinary conversation, and nested worker suppression against a local scripted provider. `--case positive --agents 3` confirms the form, creates exactly three real native worker threads, checks inherited environment values through their native execution tools, and calls the private coordination board without approval prompts, using scripted responses. It establishes the launch path, not model-generated task quality. These invocations have a 60-second ceiling and record the actual terminal bytes, native version, package hash, and result. Shutdown waits for native acknowledgement before forcing surviving owned processes. Captured terminal bytes can be rendered as an evidence image; they are not an operating-system screenshot.

The routine `test_native_selection_environment.py` regression also checks three- and four-agent confirmation with a deliberately restricted MCP environment. It verifies that the trusted command hook preserves the original environment, waits for confirmation, and starts only one runtime even if the hook is repeated. It uses the `native-lifecycle-fixture` executable and makes no host or account calls.

With explicit account authorization, the tiny live check uses GPT-6 Astra at medium effort and confirms the selected count through the native form:

```sh
python3 scripts/verify_codex_selector.py \
  --out .validation/selector-live --case live --agents 3 \
  --auth-home "${CODEX_HOME:-$HOME/.codex}"
```

The live task creates one text file and performs one focused check. Its default ceiling is 60 seconds; add `--timeout-seconds 180` for an authorized three-minute live check. Both budgets reserve time for shutdown. A timed-out or undelivered result is a failed check, not a speed result. The fixture references the existing native login without reading or copying credentials, then removes the reference after confirming shutdown.

The longer fresh-install qualification below is separate. Only run this when real account use has been authorized. Supply a small task beginning with `$delm:run `:

```sh
python3 scripts/verify_fresh_install.py \
  --out .validation/fresh-install-01 \
  --auth-home "${CODEX_HOME:-$HOME/.codex}" \
  --runtime target/debug/delm \
  --model gpt-6-astra --effort medium --agents 2 \
  --task-file /absolute/path/to/task.txt \
  --timeout-seconds 1500
```

Select an available model and effort. This helper installs the plugin into a disposable Codex home, trusts its discovered hooks there, and invokes a real parent session. It uses real account capacity and stays outside routine CI. It references the existing file-backed login without copying its contents and removes that reference after confirmed shutdown.

Evidence covers native ownership, exact task handoff, the selected worker roster, observed collaboration, delivery to the disposable original project, and removal of temporary workspaces. A `delivered` status is distinct from fully verified completion: required local setup or merged-result checks must finish in the parent before reporting the task ready. This qualifies a fresh installation, not new-account login or subjective product quality. Keep its evidence private.

### Claude Code

Use the authenticated native CLI and the staged Claude package. This small manual fixture uses the current host settings and permissions; it does not install the plugin, grant permissions, or replace the account configuration:

```sh
python3 scripts/verify_claude_native.py \
  --plugin .build/plugin-claude \
  --out .validation/claude-native-01 \
  --authorize-model-use
```

The fixture implements two small Node modules, imports a peer contribution, checks their combined output in the original project, and verifies that the original files and Git index survive and both worker trees disappear. Its work deadline is 150 seconds. After runtime completion, it allows up to 25 seconds for the parent's final handoff; a timed-out run instead enters a bounded stop period. Each invocation needs a new evidence directory. Keep transcripts and native run records private.

It records the actual host version, tool inventories, worker identities, working directories, launch timing, source and artifact hashes, and observed costs. The headless CLI sets its native fork capability switch to expose the fork available in interactive sessions; permission mode and allow rules stay unchanged. Routine CI never runs this account-backed fixture. A debug build check is development evidence; release qualification must use the exact candidate runtime and adapter bytes on each advertised architecture, as described in [releases](releases.md).

Repository preparation can also be measured without any model work:

```sh
cargo test --locked --lib preparation_timing_with_many_ignored_directories -- --ignored --nocapture
```

The fixture measures the same saved project three times. Compare like machines and build profiles; preparation time is not complete task latency.

## Contributor hygiene

Use the user's normal installed browser tools and native permissions. Do not prescribe a special Chromium sandbox workaround or disable their integrations to make a fixture pass. When browser state is shared, coordinate the relevant resource or use independent browser contexts.

Remove a Codex contributor installation with `./scripts/uninstall.sh`. Remove the Claude contributor plugin with `claude plugin uninstall delm@delm-local --scope user --keep-data`. A Claude session opened with `--plugin-dir` creates no marketplace registration to remove. Stop active DeLM work before removal, and follow [migration guidance](support.md#updating-and-removing-the-plugin) before switching a modified local installation to a published package. Make edits in this source checkout, not an installed cache.

Keep credentials, private run folders, local builds, and research evidence out of release packages. Packaging uses an explicit allowlist and records checksums. No routine qualification command commits or publishes changes.
