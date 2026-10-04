# Support and recovery

DeLM runs on macOS through the selected host's native CLI and account. Builds target macOS 13 or later on Apple Silicon and Intel; advertised release support requires qualification on the actual architecture and OS. See [development](development.md) for source builds and [releases](releases.md) for publication requirements.

## Selecting a project

Invoke `$delm:run` in Codex or `/delm:run` in Claude Code from the exact project folder you want to change. If that folder has no `.git` entry, DeLM initializes an independent repository there without creating a commit. Existing Git administration is preserved and validated. Resolve a collection containing several repositories before invoking it. Save editor buffers first: the captured baseline contains saved files, not unsaved editor content.

The input limit is 10,000,000,000 bytes, including Git history, ignored dependencies, hidden files, extended attributes, and sparse-file logical lengths. A folder at or above that limit is rejected. This is an admission limit, not a cap on later build output.

Preparation uses native macOS copy-on-write cloning on the same local filesystem. There is no full-copy fallback. Unsupported Git administration, external links in admitted source, special files, and unstable captures are rejected. Ignored files and recognized credentials are counted but omitted from worker copies; exclusions are recorded.

## Codex setup and capability inheritance

Use stock Codex CLI on `PATH` with an existing native login. A desktop or IDE installation without the CLI is insufficient.

Installation uses Codex's native plugin manager. Restart after installation, review and trust DeLM in `/hooks`, then restart to load those definitions. Invoke `$delm:run`; a bare `/delm` command is not registered.

Workers fork the parent conversation and preserve ordinary saved skills, plugins, hooks, MCP configuration, native permissions, and process environment. DeLM adds its coordination tools and prevents its own hooks from recursively launching another team. It does not substitute a stripped-down Codex setup or require a special private browser installation.

Skill contents and MCP inventories are checked rather than assuming equal names mean equal capabilities. The runtime records the inherited model, reasoning effort, and service tier. Explicit requested overrides are separate from ordinary inheritance.

**Exact live-session parity is not met yet.** A native fixture demonstrates that a parent-process CLI override is absent from a separate fork host. The host API also does not expose every live tool connection or instruction-provider state. The runtime reports these gaps. Saved configuration and a native conversation fork do not establish that those live resources are identical. Do not describe a comparison as fully matched until its capability evidence establishes that.

Account credentials remain in Codex's normal store. DeLM does not copy them into project snapshots. Missing required capabilities or inaccessible selected inputs must be surfaced rather than silently discarded.

## Claude Code setup and capability inheritance

Use Claude Code 2.1.289 or later with your normal login. The plugin uses official native skills, plugin modules, and an MCP sidecar. Invoke `/delm:run <task>`. `/delm-status` and `/delm-stop` remain available while work is running.

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

Successful delivery writes the assembled source changes into the original project. It preserves the Git index and unrelated files. Compatible concurrent text edits are merged; overlapping changes, incompatible binary edits, and file/directory type transitions are reported as conflicts.

Dependency directories are not copied wholesale. When dependency manifests change, worker-local dependency environments are omitted, or delivery merges user edits, the result has `verification_required`. The report identifies omitted environments in `environment_directories_omitted`. The parent host must perform the necessary setup or focused check in the original project before reporting the task ready. This does not require repeating an unchanged full acceptance suite.

Both worker directories and the temporary baseline are removed after confirmed shutdown and durable delivery or recovery. Cancellation saves useful partial source changes before cleanup. Conflicting or interrupted delivery retains changed-content blobs and a journal; it does not automatically overwrite the project or roll back later edits. Successful replacements also retain displaced original file inodes at the reported recovery path, preserving saves made through already-open editor descriptors. A detected concurrent change produces a recovery outcome. This is guarded per-path delivery, not a globally atomic transaction with external writers. If process ownership, storage, or cleanup cannot be confirmed, the runtime preserves what remains and reports the failure.

Ask Codex to stop, or use `/delm-stop` in Claude Code, to request cancellation. Wait for confirmed shutdown before disabling, updating, or removing the plugin. Codex cancellation hooks must remain enabled and trusted to receive native events. Claude's module must remain loaded to coordinate native agent stops. Owner checks, durable recovery records, and the execution deadline provide separate protections; they do not justify removing an active run's lifecycle integration. The ordinary deadline begins after required startup.

Run records live under `~/Library/Application Support/DeLM/runs/`. These may contain private source, conversation inputs, native events, and recovery material. Share only the redacted evidence needed for a bug report. Temporary previews stop with the run; a new preview must run from the original project.

## Updating and removing the plugin

For a published marketplace installation, the [common installer](../packages/installer/README.md#host-selection) supports `update`, `remove`, and `status` with the same host detection and choice as installation. Pass `--host codex`, `--host claude`, or `--host both` to select explicitly. You can also use the native commands below. Update and removal are separate operations. Stop active work first.

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
