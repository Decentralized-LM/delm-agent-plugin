# Support and recovery

## Selecting a project

Use `$delm:run` in one standalone Git repository. If the current directory contains several projects, identify the repository in your request. DeLM requires that exact repository root; it does not recursively clone a collection of projects. Save editor buffers first. DeLM captures saved files, not unsaved editor content.

Admission measures the entire folder, including Git history, ignored dependencies, hidden files, extended attributes, and sparse-file logical lengths. `MAX_REPO_SIZE_BYTES` in `src/config.rs` is 10,000,000,000 bytes. A folder at or above that limit is rejected. This limits the input, not later build output.

Preparation requires native copy-on-write cloning on the same local filesystem. There is no full-copy fallback. External links in admitted source files, special files, unsupported Git administration, and unstable captures are rejected. Ignored environment links are counted without following them and do not reject an otherwise valid source snapshot. Recognized local credentials and all Git-ignored untracked files are counted but omitted from worker snapshots; the saved manifest records exclusions. Workers can install missing dependencies in their private projects. Required ignored fixtures or assets can be supplied explicitly as selected inputs. They are captured as read-only references that a worker can use in its private project.

## Codex and model selection

DeLM currently supports macOS with stock Codex CLI available on `PATH`. A desktop or IDE installation without the CLI is insufficient. The release build targets macOS 13 or later on Apple Silicon and Intel; Windows and Linux are not supported yet. Native isolation has been checked locally on Apple Silicon with Codex 0.159.3 and 0.160.0. Intel and older macOS releases require release qualification before they can be described as verified.

DeLM checks the app-server methods it uses and runs a small filesystem isolation probe before starting either worker. This probe uses disposable files and generates no model turns. Additional protocol fields and newer version numbers are accepted. Missing capabilities or a failed isolation probe stop the task with an explanation, without changing Codex or starting a model turn. The protocol includes experimental features, so future compatibility cannot be guaranteed. CI checks pinned Codex versions and the latest release.

Installation uses Codex's native plugin manager, without a replacement executable or launcher. Restart an existing Codex session after installation, review and trust DeLM in `/hooks`, then restart once more so the active session loads those definitions. Then enter `$delm:run` or select **DeLM** in the skill picker. A bare `/delm` command is not registered by stock Codex plugins.

Workers use the existing native OpenAI account store. DeLM never copies account tokens into projects. The runtime displays its model and effort before work starts. An explicit model request wins; otherwise it uses the parent's persisted thread selection when available, then saved project configuration or Codex's default. Stock Codex does not expose every unsaved UI setting, so DeLM does not claim to inherit those settings exactly. The invoking skill supplies the user's task verbatim and relevant conversation constraints separately. It does not inherit or secretly summarize the complete chat, redesign the task, or assign work to the peers. Selected local images and reference files are forwarded through a bounded input manifest. Session-only attachment IDs are not portable and must be resolved before launch.

## Permissions

Starting the runtime may require Codex's normal permission for authentication and storage outside the current project sandbox. Before either worker starts task model turns, the skill confirms access through an authenticated status request. Permission delays at this stage consume no execution allowance or task model turns. Startup stops after five minutes without confirmation. No security prompt is bypassed.

Worker tools can use the network and local servers without requesting escalation. They can write only to their own private project and environment; direct access to the original project, peer workspace, runtime state, and account directories is denied. Each worker has private temporary files, XDG state, npm cache/config/install prefix, Python cache/user installation paths, and Cargo state. Python virtual environments are retained in place along with their generated and user-created files. Their interpreter links must point to qualified read-only runtimes; those dependencies are recorded and checked. Moving a retained environment to another computer may require recreating it from the project's dependency files. Installed toolchains are reused read-only; changing host toolchain installations is not supported. Use project-local dependencies or private installations instead.

Network access is available for trusted development, including local services. It is not a network isolation boundary or protection against indirect changes through host services. Workers are instructed to exchange contributions through DeLM. Local servers should bind to loopback on an available port and stop when their work finishes.

Live web search, image inspection, and native clarification questions are enabled. Unrelated hooks, plugins, external MCP servers, delegation, memory, native browser/computer-control integrations, and inherited tool environment variables remain disabled. Browser checks use a private headless installation. The tested Chromium recipe uses single-process mode because its normal multiprocess startup is blocked by macOS sandbox service registration; see [development](development.md). DeLM does not copy package registry credentials or other host secrets. Managed configuration that prevents the worker permission profile is rejected before task model turns. Ordinary Codex remains available for unsupported tools and privileged host setup.

DeLM supports trusted local development, not hostile programs deliberately escaping process ownership. It tracks native hosts and observed descendants, including new process sessions. Uncertain shutdown preserves both workspaces and prevents automatic cleanup.

## Stopping and retaining work

Ask Codex to stop to request immediate cancellation. Trusted native hooks cancel the bound invocation on interruption or normal parent completion, while process identity checks detect owner exit and changed plugin resources. Native runs have no chat heartbeat, so a slow reply does not stop healthy work. DeLM does not register SessionEnd because that event lacks an invocation identity. Stop a run before disabling DeLM or revoking its hook trust: disabled hooks cannot deliver cancellation. The execution deadline still applies and starts after preparation and control confirmation. Shutdown allows bounded time to terminate processes and save results without starting further work. Direct CLI runs outside a native Codex session retain their explicit 60-second monitoring lease.

Updates received before startup are included in the task. Once a result is accepted or stopping begins, further updates are rejected explicitly; continue from the retained project instead. Pending status requests receive the terminal outcome before the control server exits. Worker questions retain their native text and choices. Answers are correlated to one question request and then shared as context with both workers; an unrelated update never answers a question automatically.

On success, DeLM retains the completed private project and a self-contained review, then removes the unused worker and captured baseline. On interruption or uncertain shutdown, it preserves partial projects. The final status links the project, review, and recorded command checks. Preview servers are stopped before delivery. Open a preserved project to continue normally. This plugin does not automatically resume an interrupted team, apply changes to the original, or switch Codex's current project.

Run records live under `~/Library/Application Support/DeLM/runs/`. They include source snapshots, supplied context, native events, usage, board state, and result metadata. Keep these private; do not attach complete run folders to public issues. Remove saved work only when you no longer need it.

If startup fails after its native handshake was consumed, send a fresh user message before retrying. DeLM accepts one consumed launch per native turn so a delayed interruption cannot cancel a replacement run. A denied command that never started can be retried in the same turn.

## Updating and removing the plugin

For a published marketplace installation:

```sh
codex plugin marketplace upgrade delm
codex plugin remove delm@delm
```

These are separate operations: use the first to update or the second to remove. Restart Codex to load an updated skill. Native removal removes installed package files; keep personal changes outside that cache. It leaves DeLM results and Codex accounts intact.

Each new run retains its executable under `~/Library/Application Support/DeLM/runtimes/<sha256>/delm` before starting workers. The settings event and status record expose that path as `control_executable`. The skill uses it for status, clarification, and cancellation, so a native plugin upgrade or removal cannot remove a running task's control program. Old retained executables and results are not automatically deleted. Changing or removing the bound plugin stops the active run and preserves its work; the retained executable remains available to inspect it. This protection applies to runs started with this release; let older runs finish before migrating.

Once the public package is available, the npm installer will stop if a `delm-local` source installation is present, so it does not create duplicate skills. For an unmodified source installation, stop active DeLM work, run `./scripts/uninstall.sh` from its checkout, and then install the public package.

To preserve modified plugin files during migration, first install `delm@delm` through the [native marketplace instructions](releases.md#publish-and-install), then run `./scripts/migrate.sh` from the original checkout. It verifies the new registration, preserves all old cache versions and modifications under `CODEX_HOME/delm/preserved-plugins/`, and removes the checkout's `delm-local` registration. Restart Codex so only the new skill is loaded. The script stops if the new installation is missing.

Contributor installations still use `./scripts/uninstall.sh` from their source checkout. That command preserves results, accounts, and staged builds and refuses removal if installed files were modified. Migration and removal of the existing local installation are not performed by building a release.
