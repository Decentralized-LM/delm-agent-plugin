# Support and recovery

DeLM runs on macOS through the selected host's native CLI and account. Builds target macOS 13 or later on Apple Silicon and Intel; advertised release support requires qualification on the actual architecture and OS. See [development](development.md) for source builds and [releases](releases.md) for publication requirements.

The public source is available at [jerry2247/delm-agent-plugin](https://github.com/jerry2247/delm-agent-plugin). The npm package and native marketplace are not published yet; use the [source installation instructions](../CONTRIBUTING.md#install-from-source) to try the current code. After publication, `npx --yes delm-agent@latest install` will detect the available host and offer Codex, Claude Code, or both when both CLIs are installed.

## Selecting a project

Invoke `$delm:run` in Codex or `/delm:run` in Claude Code from the exact project folder you want to change. If that folder has no `.git` entry, DeLM initializes an independent repository there without creating a commit. Existing Git administration is preserved and validated. Resolve a collection containing several repositories before invoking it. Save editor buffers first: the captured baseline contains saved files, not unsaved editor content.

The input limit is 10,000,000,000 bytes, including Git history, ignored dependencies, hidden files, extended attributes, and sparse-file logical lengths. A folder at or above that limit is rejected. This is an admission limit, not a cap on later build output.

Preparation uses native macOS copy-on-write cloning on the same local filesystem. There is no full-copy fallback. Unsupported Git administration, external links in admitted source, special files, and unstable captures are rejected. Ignored files and recognized credentials are counted but omitted from worker copies; exclusions are recorded.

## Codex setup and capability inheritance

Use stock Codex CLI on `PATH` with an existing native login. A desktop or IDE installation without the CLI is insufficient.

Codex compatibility depends on its native API behavior, including how request options interact. DeLM checks protocol capabilities and resolves inherited settings before starting workers. Release qualification also exercises the actual plugin invocation, successful delivery, and return to an ordinary conversation after failure against the exact recorded host version and architecture. Installing a newer Codex version or passing a schema check alone does not establish support; see the [native verification steps](development.md#focused-verification).

Installation uses Codex's native plugin manager. Restart after installation, review and trust DeLM in `/hooks`, then restart to load those definitions. Invoke `$delm:run`; a bare `/delm` command is not registered.

Workers fork the parent conversation and preserve ordinary saved skills, plugins, hooks, MCP configuration, native permissions, and process environment. DeLM adds its coordination tools and prevents its own hooks from recursively launching another team. It does not substitute a stripped-down Codex setup or require a special private browser installation.

Skill contents and MCP inventories are checked rather than assuming equal names mean equal capabilities. The runtime records the inherited model, reasoning effort, and service tier. Explicit requested overrides are separate from ordinary inheritance.

Before taking the source inventory and preparing worker forks, DeLM waits for Codex's native plugin-bundle initialization. A failed initialization is reported before worker execution. This boundary does not certify that every tool is connected or ready; the subsequent native settings, skill-content, and MCP-inventory checks remain authoritative.

**Exact live-session parity is not met yet.** A native fixture demonstrates that a parent-process CLI override is absent from a separate fork host. The host API also does not expose every live tool connection or instruction-provider state. The runtime reports these gaps. Saved configuration and a native conversation fork do not establish that those live resources are identical. Do not describe a comparison as fully matched until its capability evidence establishes that.

Account credentials remain in Codex's normal store. DeLM does not copy them into project snapshots. Missing required capabilities or inaccessible selected inputs must be surfaced rather than silently discarded.

## Claude Code setup and capability inheritance

Use Claude Code 2.1.289 or later with your normal login. The plugin uses official native skills, plugin modules, and an MCP sidecar. Invoke `/delm:run <task>` to start work and open the live board. It appears on the right in fullscreen terminals at least 110 columns wide, or above the normal prompt in narrower terminals and the classic renderer.

Click a task or shared entry for details; **Back** returns to the overview. **Hide board** closes the view while work continues. Reopen it with `/delm-status` or **Show board** in the compact status. Opening and updating the board add no model calls. Keep typing follow-ups in the normal prompt, and answer questions or permissions through Claude's normal interface. When a peer finishes a turn, Claude's notice for it shows as a one-line note instead of a parent reply; the board shows the run's progress. `/delm-stop` requests cancellation; the board shows **Stopping** until shutdown and saving are confirmed.

The final board distinguishes applied changes, required local verification, recovery, and cleanup. When local verification is required, use Claude's final handoff for its outcome; the board does not observe the parent's later checks. If updates disconnect, the last snapshot remains marked as disconnected with the reason the board reported, which does not mean the run stopped. `/delm-status` retries the view and provides a short text summary if the pane is unavailable. Merely opening or hiding the board does not recover, resume, or cancel a run. **Retry finishing**, when offered, explicitly retries shutdown and delivery of the selected result; `/delm-stop` instead cancels and saves partial work.

Ordinary prompts are forwarded to the peers only while DeLM is actively working. Finishing, stopping, failed and recovery states release the normal Claude conversation, including after a restart. You can ask Claude to investigate while the run's work and recovery controls remain preserved. An update that loses the finalization race is reported as undelivered; it is not silently replayed as a new task. Starting another DeLM run in the same project still requires resolving the previous run's ownership.

Two native conversation forks inherit the current session's model, system prompt, history, and available tools. A short parent launch turn makes the Agent calls through Claude's ordinary permissions. Task updates reach both peers under their existing identities. An active peer receives the update with plugin provenance and starts a fresh native turn before acknowledging the new revision; native SendMessage resumes peers when needed. DeLM does not change the permission mode or add allow rules. Claude surfaces background-agent permission prompts in the main session through its [native Agent behavior](https://code.claude.com/docs/en/tools-reference).

DeLM's board tools are additional MCP capabilities with their own native permission checks. Their implementation confines file exchange to the prepared private workspaces; delivery applies the accepted source changes to the selected original project. Rules for individual native Read or Edit tools are not a filesystem sandbox around plugin code. Review permissions for DeLM's MCP tools themselves, and use this plugin only with trusted local projects. Native tools used by the workers retain their usual permission handling.

Completion capture can validate the external Python interpreter of a real worker-local virtual environment. It checks the interpreter, configuration, and link structure without permitting general external source links; the original project and private run storage remain excluded. This is part of the DeLM MCP capability, not an inferred native Read grant. Dependency environments are omitted from original-project delivery.

The host records actual Bash tool outcomes. Claude does not expose a numeric exit code in this interface, so receipts preserve native success or failure without inventing one. Interrupted, background, timed-out, and unobserved results cannot qualify as completed checks.

Cancellation stops only recorded DeLM agents and their tracked background tasks. Cleanup also checks registered preview processes and contemporaneous workspace references. If shutdown cannot be established, the private copies remain available for recovery. Restarting or reloading the plugin recovers recorded native ownership before admitting another run. An interrupted capture whose ownership cannot be reconstructed needs manual recovery; it is never removed based on a guessed directory name.

## Permissions and shared resources

Workers retain the native permission and approval policy. Private working directories separate their edits; they are not a new security boundary overriding that policy. Answer native approval requests through the host. Codex board transfers additionally apply the exported filesystem profile; Claude board tools follow the MCP permission and containment rules described above.

Preview ownership is coordinated through the service registry. Workers claim a service, start it using normal native tools on an available loopback port, and register the actual listener. The runtime verifies process ownership and the bound port. Repeated claims reuse the existing registration. A conflicting port never authorizes stopping an unrelated process.

Separate browser contexts or test data allow independent checks against the same preview. A shared mutable scenario uses a short check claim. Check receipts apply to the recorded scoped files and revision, not every future state of a running development server.

## Results and recovery

Successful delivery writes the assembled source changes and declared requested artifacts into the original project. Workers declare project-relative output paths at completion, including files in ignored output directories, or explicitly declare a source-only result. The same accepted file contract is checked after shutdown and used for delivery. Additional custom output is saved for review; incidental build files do not turn an otherwise successful, fully declared result into a failure. If the worker never accounts for requested artifacts, delivery reports that uncertainty. Disposable ignored caches and dependency environments are separate from accepted outputs. Delivery preserves the Git index and unrelated files. Compatible concurrent text edits are merged; overlapping changes, incompatible binary edits, and file/directory type transitions are reported as conflicts.

Dependency directories are not copied wholesale. When dependency manifests change, worker-local dependency environments are omitted, or delivery merges user edits, the result has `verification_required`. The report identifies omitted environments in `environment_directories_omitted`. The parent host must perform the necessary setup or focused check in the original project before reporting the task ready. This does not require repeating an unchanged full acceptance suite.

Both worker directories and the temporary baseline are removed after confirmed shutdown and durable delivery or recovery. Cancellation saves useful partial source changes before cleanup. Conflicting or interrupted delivery retains changed-content blobs and a journal; it does not automatically overwrite the project or roll back later edits. Successful replacements also retain displaced original file inodes at the reported recovery path, preserving saves made through already-open editor descriptors. A detected concurrent change produces a recovery outcome. This is guarded per-path delivery, not a globally atomic transaction with external writers. If process ownership, storage, or cleanup cannot be confirmed, the runtime preserves what remains and reports the failure.

Ask Codex to stop, or use `/delm-stop` in Claude Code, to request cancellation. Wait for confirmed shutdown before disabling, updating, or removing the plugin. Codex cancellation hooks must remain enabled and trusted to receive native events. Claude's module must remain loaded to coordinate native agent stops. Owner checks, durable recovery records, and the execution deadline provide separate protections; they do not justify removing an active run's lifecycle integration. The execution allowance starts after project preparation: Codex includes native initialization and worker admission, while Claude includes the native launch after its ready contract. It is not a guarantee of that many minutes of model execution.

Run records live under `~/Library/Application Support/DeLM/runs/`. These may contain private source, conversation inputs, native events, and recovery material. Share only the redacted evidence needed for a bug report. Temporary previews stop with the run; a new preview must run from the original project.

## Inspecting runs and collecting a support report

The native runtime provides local support commands for both hosts. From a source checkout after `./scripts/build.sh --host all`, use:

```sh
./.build/plugin/bin/delm runs
./.build/plugin/bin/delm runs --json
./.build/plugin/bin/delm report --run-id <RUN_ID>
./.build/plugin/bin/delm report --run-id <RUN_ID> --output delm-report.json
```

Both packages contain the same runtime; a Claude-only source build can use `./.build/plugin-claude/bin/delm`. For an installed plugin, use its current runtime executable. Codex records the retained executable as `control_executable`; Claude records it as `ready.executable` in native control state. These commands require a runtime containing the support commands; use the current build to inspect records created by older releases. These support commands do not start a model, renew a run's monitoring lease, or alter the project. `runs` shows local project paths so you can identify work; use `report`, rather than the run listing or raw state, when sharing diagnostics.

The report uses an explicit export allowlist: host, recorded versions where available, run identity, finalization stage and reason, delivery flags, counts, and timing measurements. Missing historical fields remain unavailable; the reporting binary's version is separate from the version that ran the task. Reports exclude project paths, source, prompts, command text, native output, credentials, and recovery contents. Output files are created with owner-only permissions; an existing file is never overwritten.

Timing separates preparation, worker admission, shutdown, delivery with cleanup, and recovery with cleanup where recorded. Native worker-turn overlap includes model execution, tools, and host waits; it is not a direct measure of useful work or a speedup claim. Waiting durations count completed waits, and open intervals are identified separately. Older or partial evidence leaves unavailable metrics empty. Parent work after runtime completion is outside this measurement. Recorded coordination-response sizes and repeated checks with identical commands, scoped inputs, and request revisions help identify overhead; repeated evidence alone does not establish unnecessary testing or equivalent environments.

Storage sizes are logical file lengths. APFS copy-on-write sharing means they do not predict how much physical disk space deletion would recover.

## Exporting saved partial changes

Inspect a completed recovery bundle, then export one worker's changes into a new folder:

```sh
./.build/plugin/bin/delm recover --run-id <RUN_ID>
./.build/plugin/bin/delm recover --run-id <RUN_ID> --worker 1 --output ../delm-recovered
```

The export verifies saved content hashes and writes `files/` for changed results, `base/` for their original versions, and `manifest.json` for deletions, file metadata and inert symlink targets. It is a set of partial changes: unchanged project files are not included. Review it against your current project before applying anything. The command never overwrites an existing destination or changes the original project. The output contains local paths and file metadata; use `report` for shareable diagnostics.

An incomplete bundle is refused while its remaining workspaces stay preserved. Delivery journals can also contain displaced originals from successful or interrupted application; those are retained independently and are not presented as a completed partial-change export.

## Cleaning verbose diagnostics

Preview cleanup for a specific successfully delivered run, then explicitly confirm:

```sh
./.build/plugin/bin/delm clean --run-id <RUN_ID>
./.build/plugin/bin/delm clean --run-id <RUN_ID> --confirm
```

The preview lists the exact diagnostic files and logical bytes eligible for removal. Confirmation removes only the verbose event journal and worker capability reports listed by the command. A compact privacy-safe timing report is retained first. The run's ownership records, selected completion evidence, shared board, delivery journal, recovery contents, and executable are preserved, along with every file in the original project.

Cleanup requires confirmed delivery, completed workspace cleanup, confirmed native shutdown, an exited runtime, and the project's exclusive maintenance lock. Active, incomplete, conflicting, or uncertain runs are refused. Resolve those runs through their host's stop and recovery path first. This command does not discard partial work or displaced original file contents; those can still matter when recovering interrupted delivery or an editor save.

## Updating and removing the plugin

For a published marketplace installation, the [common installer](../packages/installer/README.md#host-selection) supports `update`, `remove`, and `status` with the same host detection and choice as installation. Pass `--host codex`, `--host claude`, or `--host both` to select explicitly. You can also use the native commands below. Update and removal are separate operations. Stop active work first.

```sh
npx --yes delm-agent@latest status
npx --yes delm-agent@latest update
npx --yes delm-agent@latest remove
```

The common installer and source activation scripts refuse active or uncertain runs. A fully stopped run with no temporary workspaces and a verified recovery bundle can remain saved while you update or remove the plugin. Saved work is not deleted by maintenance. These checks are preflight checks, so do not start a new run concurrently with maintenance.

If the repository moves, a new installer release can recognize explicitly approved former GitHub addresses. An existing native registration keeps its old URL and follows GitHub's transfer redirect; fresh installations use the new URL. This requires an actual repository transfer or rename and a working redirect. Copying the code into another repository does not provide that behavior. See the [maintainer transfer procedure](releases.md#transfer-the-repository-later). Unrelated marketplace registrations are not replaced automatically.

### Codex

Update:

```sh
codex plugin marketplace upgrade delm
```

Remove:

```sh
codex plugin remove delm@delm
```

Restart Codex after an update and review changed hooks when requested.

### Claude Code

Update the marketplace and installed user-scoped plugin:

```sh
claude plugin marketplace update delm
claude plugin update delm@delm --scope user
```

Remove while retaining saved plugin data:

```sh
claude plugin uninstall delm@delm --scope user --keep-data
```

Restart Claude Code after an update. These commands manage a user-scoped installation; review any different or duplicate scope through Claude's native plugin manager.

### Retained state and contributor installations

Each run retains its executable under `~/Library/Application Support/DeLM/runtimes/<sha256>/delm`. Codex events expose it as `control_executable`; Claude stores the retained path with its native control state. Status, cancellation, and recovery use the retained runtime instead of assuming the installed package has stayed unchanged. Codex detects changed or removed bound package resources and stops its run; Claude reload recovery confirms recorded native ownership has stopped before removing workspaces. Uninstalling does not erase DeLM run records or host account credentials. The [common installer](../packages/installer/README.md) delegates maintenance to the selected host and preserves its marketplace registration.

Remove a Codex contributor installation with `./scripts/uninstall.sh` from its checkout. The script refuses to discard modified installed plugin files. To preserve a modified Codex installation during migration, first install `delm@delm` using the [native marketplace instructions](releases.md#publish-and-install), then run `./scripts/migrate.sh` from the old checkout. It verifies the new registration, saves old cache versions under `CODEX_HOME/delm/preserved-plugins/`, and removes the local registration. Restart afterward.

A Claude source installation uses the local marketplace described in [development](development.md#claude-code-plugin). It reads the staged package in place: stop active work, rebuild with `./scripts/build.sh --host claude`, and restart Claude Code to load changes. Keep the checkout available while the installation is registered. Remove it with:

```sh
claude plugin uninstall delm@delm-local --scope user --keep-data
```

The optional `claude plugin marketplace remove delm-local` command removes its catalog registration. A package loaded only with `--plugin-dir` is session-local and creates no marketplace registration to remove. The Codex uninstall and migration scripts do not apply to Claude.

For either host, the common installer refuses an installed `delm@delm-local` plugin during installation or update rather than creating duplicate skills. An empty local marketplace alone does not block it.

Building or testing a release does not migrate the installed plugin or publish a package.
