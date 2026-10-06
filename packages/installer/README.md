# DeLM installer

This source package is **unpublished**, has not reserved its npm name, and has `private: true` to prevent accidental publication. Its `release.json` deliberately has no repository configured. Management commands refuse to run until a release package is prepared; help and version remain available. The signed native plugin marketplace has not been published either. The command below is the planned public interface; it is not an available install command yet:

```sh
npx --yes delm-agent@latest install
```

The installer is a small, dependency-free Node CLI. It detects the installed host and delegates installation, updates, removal, and status to its native plugin manager. When both Codex and Claude Code are available, you choose one or both. A prepared package installs `delm@delm` from its configured repository's `marketplace` branch, using that host's native catalog. Preparation takes one GitHub `OWNER/REPO` destination and fixes it inside that package; users cannot override it with a CLI flag. The installer does not replace either host, edit configuration files itself, grant permissions, start workers, or run an updater. The installer version is independent of the plugin version; a new plugin release does not require publishing a new installer.

## Requirements and commands

Installation currently supports macOS. Node.js 22+, Git, and the selected host CLI are required. Claude Code 2.1.289 or newer is the supported version floor for the native fork/Mods API contract. Install and update check that version before making changes; status and removal remain available on older versions when their native plugin JSON is compatible. For Codex, a desktop or IDE installation without stock Codex CLI is insufficient. Complete account login through the selected host before running DeLM; this installer does not inspect credentials. In a prepared package, `--help`, `--version`, and read-only `status` are available on other operating systems. Users need Git access to its distribution repository; publishing npm does not make a private Git repository publicly accessible.

Once published, use these commands without a global installation. They are not available from npm yet:

```sh
npx --yes delm-agent@latest install
npx --yes delm-agent@latest status
npx --yes delm-agent@latest update
npx --yes delm-agent@latest remove
```

## Host selection

All four commands use the same selection rules:

- When only one host CLI is available on `PATH`, the installer selects it automatically.
- When both are available, an interactive terminal offers **Codex**, **Claude Code**, or **Both**. There is no preselected answer; cancelling exits before changing either host.
- When neither is available, the installer explains which CLI to install before trying again.

For a script or a specific target, choose explicitly:

```sh
npx --yes delm-agent@latest install --host codex
npx --yes delm-agent@latest install --host claude
npx --yes delm-agent@latest install --host both
```

`--host` also applies to update, removal, and status. When both CLIs are detected, noninteractive commands and `--json` require an explicit selection instead of guessing. The `--yes` option belongs to npm and suppresses its package-download confirmation; it does not choose a DeLM host.

Use `--codex PATH` or `--claude PATH` to select a custom executable, including a relative path. Each option implies its host if `--host` is omitted; supplying both implies both hosts. An option that conflicts with an explicit single host is rejected. All native commands run in temporary directories away from project-specific configuration. Existing `CODEX_HOME` and `CLAUDE_CONFIG_DIR` selections are respected.

Selecting both runs the native operations in sequence and reports each outcome. A failure in one host does not undo a successful operation in the other, and the overall command exits with an error if either fails. Retry only the failed host with its explicit flag. Single-host `--json` output retains its `host` field; a both-host response contains `command`, `success`, `results`, and `errors`, with each error identifying its host and code.

## Activation and maintenance

After installation, restart Codex, open `/hooks`, and review and trust the DeLM hooks when requested. Restart again after granting trust, then invoke `$delm:run <task>`. Updates retain Codex's native hook-review requirements. Stop active DeLM work before updating or removing its plugin. Removal retains saved work, accounts, unrelated plugins, and the marketplace registration.

For Claude Code, restart to load the plugin, then invoke `/delm:run <task>`. The installer explicitly manages user scope. It validates the marketplace URL and branch and refuses duplicate or other-scope DeLM registrations. Removal calls native `uninstall --keep-data`, retaining the plugin's persistent data and marketplace. Claude's native uninstall can remove the plugin's stored options and secrets; host login credentials are separate and untouched. Native policy refusals remain refusals; the installer never automatically accepts marketplace shell commands or grants tool permissions.

Repeating `install` leaves an enabled plugin in place and reports that no reinstall was needed. Installation status does not verify hook trust in your current session. `status` distinguishes the public plugin from a source installation, and `remove` reports when a source installation remains. Updating a disabled plugin keeps it disabled; use `install` when you want to enable it.

`status --json` includes a `readiness` object with the installation state, `session: "not_checked"`, and host-specific next steps. An enabled installation is distinct from a running session that has loaded the plugin and accepted its native trust requirements.

Before each native mutation, the installer checks local DeLM run records for the selected host. Active runs, uncertain records, and remaining worker directories block maintenance with a specific recovery message. This is a preflight check, not a lock on other host sessions: stop DeLM first and keep it stopped until maintenance finishes. Read-only `status` remains available. The installer never removes run records to make an update proceed.

A conflicting `delm` marketplace is preserved and blocks changes. An installed `delm@delm-local` plugin blocks install and update; explicit removal of a verified public plugin remains available and preserves the legacy plugin. An empty `delm-local` marketplace does not block installation. For an ordinary source installation, stop active work, run `./scripts/uninstall.sh` from its original checkout, then retry. That script preserves results and refuses removal of modified cached files. To preserve modified cache contents, use the repository's documented native public-install-then-`migrate.sh` path once a release is available; the npm installer does not automate migration.

For an existing Claude `delm@delm-local` source plugin, inspect and manage it through Claude's native plugin manager; the Codex source uninstall script does not apply. An installed source plugin blocks Claude install/update but is preserved by removal of a verified public plugin.

Failures are reported with native host details. If registration succeeds but installation fails, the registration remains available for inspection and retry. The installer does not automatically remove partial state or unrelated registrations. Claude mutation results are parsed from their final JSON line; list results use their documented JSON arrays. State is verified after every operation.

## Repository transfers

The first distribution will use `jerry2247/delm-agent-plugin`. A later GitHub
transfer or rename does not change the `delm-agent` npm package name. Prepare a
new installer version with the new destination and add each previous address
with `--previous-repository OWNER/REPO`. New installations use the new address;
existing native marketplace registrations may retain an explicitly approved
previous address and follow GitHub's redirect. Copying the code into a different
repository does not create that redirect. Keep previous repository paths unused
so another repository cannot replace them.

Only addresses listed in the released package are accepted. A similarly named
marketplace at another source remains a conflict. This support does not migrate
`delm-local` source installations or alter plugin permissions.

## Verification

From this package directory:

```sh
npm test
npm run test:native
npm run test:native:claude
```

Private tests also require Python 3 and npm. The regular tests exercise host discovery and selection, cancellation, noninteractive output, partial failures, prerequisites, native conflict handling, idempotency, updates, removals, preparation, the package allowlist, and an offline npm publication dry run. Native tests additionally require macOS, Git, npm/npx, and stock Codex CLI. They prepare a package for a test repository, run its generated tarball through local `npx`, and redirect that GitHub URL to a disposable local Git fixture using an isolated Git configuration. They install no global packages, use no credentials or model calls, and do not change the user's Codex home. `DELM_TEST_HOST` can select an existing Codex executable for qualification.

`test:native:claude` uses the same disposable Git transport with a separate temporary `HOME` and `CLAUDE_CONFIG_DIR`. It verifies install, repeated install, disabled update, enable, removal, data retention, unrelated settings/plugins, and wrong-branch rejection through the real Claude CLI. It uses no model calls or account login. `DELM_TEST_CLAUDE` selects an existing executable. This qualifies native distribution, separately from the runtime's worker behavior.

To prepare a release installer, run this from the repository root with the intended distribution destination and a fresh output directory:

```sh
python3 scripts/prepare_installer.py --repository OWNER/REPO --out .validation/prepared-installer
```

Optionally add `--native-release /path/to/native/release.json` to require the same repository and bind that native release's metadata hash and source provenance in `preparation.json`. Preparation does not qualify or publish a native release. The installer version comes from this package's `package.json`, independently of the native version.

The output contains `package/`, `delm-agent-<version>.tgz`, `preparation.json`, and `SHA256SUMS`. The generated package has the chosen repository, public-use help and README, and `private: false`; the source files remain unchanged and protected. The checksum file covers the tarball and preparation record so it can be verified with those files alone. The preparation record also hashes each packaged file. Reusing an output directory is refused.

The tarball contains only the CLI, implementation, release configuration, package metadata, README, license, and notice. Packing uses isolated npm configuration and no lifecycle scripts or registry access. Publish the generated tarball only after separately confirming the npm name and publishing a qualified, accessible native plugin release. Do not remove the source package's publication guard.

Mechanics follow [npm executable and package metadata](https://docs.npmjs.com/cli/v11/configuring-npm/package-json), [npm exec/npx arguments](https://docs.npmjs.com/cli/v11/commands/npm-exec), and Codex's native [marketplace](https://github.com/openai/codex/blob/main/codex-rs/cli/src/marketplace_cmd.rs) and [plugin](https://github.com/openai/codex/blob/main/codex-rs/cli/src/plugin_cmd.rs) commands.

Claude's adapter follows its official [plugin commands](https://code.claude.com/docs/en/plugins/cli-reference) and [marketplace source format](https://code.claude.com/docs/en/plugins/marketplace-reference). Both installation scopes and saved data follow the native host's rules.
