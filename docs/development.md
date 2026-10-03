# Development

Use macOS with stock Codex CLI on `PATH`, an existing Codex login, Git, Python 3, Xcode Command Line Tools, and the Rust toolchain pinned in `rust-toolchain.toml`.

From this checkout, build and install through the native plugin manager:

```sh
./scripts/install.sh
```

Restart Codex, open `/hooks`, and review and trust the DeLM hooks. Restart Codex once more, then open a Git repository and invoke `$delm:run <task>`. The local marketplace is named `delm-local`, keeping contributor builds separate from public `delm` releases. The installer does not replace Codex, start a task, or grant hook trust. After an update, review changed hooks when Codex requests it and restart before running DeLM.

To build without installing:

```sh
./scripts/build.sh
```

The package is staged under `.build/plugin/`. Previous packages are preserved. To stage an existing compatible runtime without compiling, use `./scripts/build.sh --prebuilt /absolute/path/to/delm`.

Run the automated checks:

```sh
./scripts/verify.sh
DELM_TEST_HOST="$(command -v codex)" cargo test --locked --test native_sandbox -- --ignored
node --test examples/task-board/task.test.mjs
```

The regular suite uses fixture workers. Installation tests use temporary Codex homes and local Git repositories. The native suite checks filesystem restrictions for both workers and installs tiny npm and Python dependencies from a temporary local server, without generating model turns. It needs Node/npm and Python virtual environment support. Run it from a normal terminal, since an enclosing sandbox can prevent local sockets or nested sandbox execution. None of these checks launches a real DeLM task.

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
