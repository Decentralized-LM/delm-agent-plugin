# Development

Use macOS with stock Codex CLI on `PATH`, an existing Codex login, Git, Python 3, Xcode Command Line Tools, and the Rust toolchain pinned in `rust-toolchain.toml`. The complete test suite also needs Node.js 22 or later and npm.

From this checkout, build and install through the native plugin manager:

```sh
./scripts/install.sh
```

The installer checks macOS support, the existing Codex home, native plugin commands, Git, Rust/Cargo, and the Xcode compiler before building or changing plugin registrations. Missing prerequisites produce setup guidance. It checks command capabilities rather than requiring a specific Codex version. A staged install with `--no-build` does not require Rust or Xcode; removal and migration do not require build tools either. These checks do not inspect account credentials or start model turns; use Codex itself to complete login before your first DeLM run.

Restart Codex, open `/hooks`, and review and trust the DeLM hooks. Restart Codex once more, then open a Git repository and invoke `$delm:run <task>`. The local marketplace is named `delm-local`, keeping contributor builds separate from public `delm` releases. The installer does not replace Codex, start a task, or grant hook trust. After an update, review changed hooks when Codex requests it and restart before running DeLM.

To build without installing:

```sh
./scripts/build.sh
```

The package is staged under `.build/plugin/`. Previous packages are preserved. To stage an existing compatible runtime without compiling, use `./scripts/build.sh --prebuilt /absolute/path/to/delm`.

Run the automated checks:

```sh
./scripts/verify.sh
npm --prefix packages/installer run test:native
DELM_TEST_HOST="$(command -v codex)" cargo test --locked --test native_sandbox -- --ignored
node --test examples/task-board/task.test.mjs
```

The regular suite uses fixture workers. Installation tests use temporary Codex homes and local Git repositories. The native suite checks filesystem restrictions for both workers and installs tiny npm and Python dependencies from a temporary local server, without generating model turns. It needs Node/npm and Python virtual environment support. Run it from a normal terminal, since an enclosing sandbox can prevent local sockets or nested sandbox execution. None of these checks launches a real DeLM task.

The [npm installer](../packages/installer/README.md) has no dependencies or installation scripts. Its version is independent of the plugin version. Unit and package tests run in the regular suite; `test:native` exercises the packed command against stock Codex and a local Git marketplace in temporary homes. It does not register a plugin in your normal Codex configuration or contact a published DeLM marketplace.

The [native lifecycle checks](native-lifecycle-qualification.md) cover interruption, cancellation during startup, normal chat completion, owner-process death, and plugin removal. CI runs these with each Codex version in its matrix, using disposable Codex homes and a local scripted provider.

For an explicitly authorized real-model check, put a small development task beginning with `$delm:run ` in a text file, then run:

```sh
python3 scripts/verify_fresh_install.py \
  --out .validation/fresh-install-01 \
  --auth-home "${CODEX_HOME:-$HOME/.codex}" \
  --runtime target/debug/delm \
  --model gpt-6-astra --effort xhigh \
  --task-file /absolute/path/to/task.txt \
  --timeout-seconds 1500
```

Select a model and effort available to your account. Use a new output directory and allow enough time for startup, the task, and shutdown; reaching the helper's timeout interrupts the task. This check uses real account capacity and is excluded from routine CI. It installs the plugin in a disposable Codex home, trusts only its discovered hooks there, and invokes the installed skill through a real native parent. It reuses an existing file-backed Codex login through a temporary reference, which is removed after verified process shutdown. This qualifies a fresh plugin and configuration setup, not new-account authentication. Evidence records task handoff, both workers, observed collaboration, retained output, original-project integrity, and cleanup. Keep that evidence private.

To measure repository preparation independently of model work:

```sh
cargo test --locked --lib preparation_timing_with_many_ignored_directories -- --ignored --nocapture
```

This fixture reports three measurements for 400 ignored directories, 40 empty directories, and a 4 MiB saved file. Compare the same machine and build profile; this measures snapshot preparation, not complete task latency.

Set `DELM_TEST_PUBLIC_NETWORK=1` for the native suite to also download and execute a pinned npm dependency from the public HTTPS registry. This optional check uses disposable worker projects and does not run package lifecycle scripts. Normal CI remains independent of public package downloads during qualification.

For optional browser qualification, set `DELM_TEST_BROWSER_BUNDLE` to an existing Playwright Chromium headless-shell directory containing `chrome-headless-shell` and run the native suite. The test clones that bundle into its disposable project, renders a canvas page, and verifies the screenshot. It uses a private browser profile and never opens an existing browser or profile. Both workers retain the same Codex filesystem restrictions used during tasks.

Within a worker, Playwright browser downloads use its private `PLAYWRIGHT_BROWSERS_PATH`. A headless Chromium launch can use:

```js
const browser = await chromium.launch({
  headless: true,
  args: ['--single-process', '--no-zygote', '--no-sandbox'],
});
```

These arguments were qualified with stock Codex 0.159.3 on Apple Silicon. Multiprocess Chromium cannot register its Mach service within that native sandbox. `--no-sandbox` disables Chromium's inner sandbox only; Codex still enforces worker filesystem isolation. This recipe supports private browser checks, not attachment to the user's browser. The native test verifies rendering and screenshot reads without model calls, so it does not establish model visual judgment. Workers have the native `view_image` capability enabled for their own screenshots.

CI runs on Apple Silicon and Intel with pinned and latest Codex versions. This checks compatibility when Codex changes rather than silently downgrading a user's installation. Release builds and signing are described in [releases](releases.md).

Remove a contributor installation with `./scripts/uninstall.sh`. To move an existing contributor installation to a published release, follow [migration](support.md#updating-and-removing-the-plugin). Do not edit installed plugin files; make changes in the source checkout.

Keep tests, local builds, credentials, product research, and design drafts out of release packages. Packaging uses an explicit file list and records checksums. Test changes to worker behavior separately from installation changes.
