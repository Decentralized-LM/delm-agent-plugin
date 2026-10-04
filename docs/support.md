# Support and recovery

## Selecting a project

Invoke `$delm:run` from the exact project folder you want to change. If that folder has no `.git` entry, DeLM initializes an independent repository there without creating a commit. Existing Git administration is preserved and validated. Resolve a collection containing several repositories before invoking it. Save editor buffers first: the captured baseline contains saved files, not unsaved editor content.

The input limit is 10,000,000,000 bytes, including Git history, ignored dependencies, hidden files, extended attributes, and sparse-file logical lengths. A folder at or above that limit is rejected. This is an admission limit, not a cap on later build output.

Preparation uses native macOS copy-on-write cloning on the same local filesystem. There is no full-copy fallback. Unsupported Git administration, external links in admitted source, special files, and unstable captures are rejected. Ignored files and recognized credentials are counted but omitted from worker copies; exclusions are recorded.

## Codex setup and capability inheritance

DeLM needs stock Codex CLI on `PATH` and an existing native login. A desktop or IDE installation without the CLI is insufficient. Builds target macOS 13 or later on Apple Silicon and Intel; release support requires qualification on the advertised architecture and OS.

Installation uses Codex's native plugin manager. Restart after installation, review and trust DeLM in `/hooks`, then restart to load those definitions. Invoke `$delm:run`; a bare `/delm` command is not registered.

Workers fork the parent conversation and preserve ordinary saved skills, plugins, hooks, MCP configuration, native permissions, and process environment. DeLM adds its coordination tools and prevents its own hooks from recursively launching another team. It does not substitute a stripped-down Codex setup or require a special private browser installation.

Skill contents and MCP inventories are checked rather than assuming equal names mean equal capabilities. The runtime records the inherited model, reasoning effort, and service tier. Explicit requested overrides are separate from ordinary inheritance.

**Exact live-session parity is not met yet.** A native fixture demonstrates that a parent-process CLI override is absent from a separate fork host. The host API also does not expose every live tool connection or instruction-provider state. The runtime reports these gaps. Saved configuration and a native conversation fork do not establish that those live resources are identical. Do not describe a comparison as fully matched until its capability evidence establishes that.

Account credentials remain in Codex's normal store. DeLM does not copy them into project snapshots. Missing required capabilities or inaccessible selected inputs must be surfaced rather than silently discarded.

## Permissions and shared resources

Workers retain the native permission and approval policy. Private working directories separate their edits; they are not a new security boundary overriding that policy. Native approval requests must be answered by the user through the parent conversation. DeLM's privileged board transfers apply separate path-containment and native-policy checks.

Preview ownership is coordinated through the service registry. Workers claim a service, start it using normal native tools on an available loopback port, and register the actual listener. The runtime verifies process ownership and the bound port. Repeated claims reuse the existing registration. A conflicting port never authorizes stopping an unrelated process.

Separate browser contexts or test data allow independent checks against the same preview. A shared mutable scenario uses a short check claim. Check receipts apply to the recorded scoped files and revision, not every future state of a running development server.

## Results and recovery

Successful delivery writes the assembled source changes into the original project. It preserves the Git index and unrelated files. Compatible concurrent text edits are merged; overlapping changes, incompatible binary edits, and file/directory type transitions are reported as conflicts.

Dependency directories are not copied wholesale. When dependency manifests change, worker-local dependency environments are omitted, or delivery merges user edits, the result has `verification_required`. The report identifies omitted environments in `environment_directories_omitted`. Codex must perform the necessary setup or focused check in the original project before reporting the task ready. This does not require repeating an unchanged full acceptance suite.

Both worker directories and the temporary baseline are removed after confirmed shutdown and durable delivery or recovery. Cancellation saves useful partial source changes before cleanup. Conflicting or interrupted delivery retains changed-content blobs and a journal; it does not automatically overwrite the project or roll back later edits. Successful replacements also retain displaced original file inodes at the reported recovery path, preserving saves made through already-open editor descriptors. A detected concurrent change produces a recovery outcome. This is guarded per-path delivery, not a globally atomic transaction with external writers. If process ownership, storage, or cleanup cannot be confirmed, the runtime preserves what remains and reports the failure.

Ask Codex to stop for immediate cancellation. Keep DeLM enabled and its hooks trusted until an active run has stopped. Disabling native hooks prevents them from delivering cancellation; owner checks and the execution deadline remain separate protections. The ordinary deadline begins after required startup.

Run records live under `~/Library/Application Support/DeLM/runs/`. These may contain private source, conversation inputs, native events, and recovery material. Share only the redacted evidence needed for a bug report. Temporary previews stop with the run; a new preview must run from the original project.

## Updating and removing the plugin

For a published marketplace installation:

```sh
codex plugin marketplace upgrade delm
codex plugin remove delm@delm
```

These are separate operations: the first updates; the second removes. Restart Codex after an update and review changed hooks when requested. Stop active work before removal.

Each run retains its executable under `~/Library/Application Support/DeLM/runtimes/<sha256>/delm`. Events expose it as `control_executable`, allowing status and cancellation even if the installed plugin changes. A changed or removed bound plugin stops the active run and preserves useful work. Existing run records and account credentials are not removed by uninstalling.

Contributor installations use `./scripts/uninstall.sh` from their checkout. The script refuses to discard modified installed plugin files. Once the public package exists, its installer rejects an existing `delm-local` registration rather than creating duplicate skills.

To preserve a modified local installation during migration, first install `delm@delm` using the [native marketplace instructions](releases.md#publish-and-install), then run `./scripts/migrate.sh` from the old checkout. It verifies the new registration, saves old cache versions under `CODEX_HOME/delm/preserved-plugins/`, and removes the local registration. Restart afterward.

Building or testing a release does not migrate the installed plugin or publish a package.
