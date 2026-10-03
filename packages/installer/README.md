# DeLM installer

This package is **unpublished**, has not reserved its npm name, and has `private: true` to prevent accidental publication. The signed native plugin marketplace has not been published either. The command below is the planned public interface; it is not an available install command yet:

```sh
npx --yes delm-agent@latest install
```

The installer is a small, dependency-free Node CLI. It delegates installation, updates, removal, and status to stock Codex's native plugin manager. It installs `delm@delm` from the `marketplace` branch of `jerry2247/delm-agent-plugin`. It does not replace Codex, edit configuration files itself, grant hook trust, start workers, or run an updater. The installer version is independent of the plugin version; a new plugin release does not require publishing a new installer.

## Requirements and commands

Installation currently supports macOS. Node.js 22+, stock Codex CLI, and Git are required. A desktop or IDE installation without Codex CLI is insufficient. Complete account login through Codex before running DeLM; this installer does not inspect credentials. `--help`, `--version`, and read-only `status` are available on other operating systems.

Once published, use these commands without a global installation. They are not available from npm yet:

```sh
npx --yes delm-agent@latest install
npx --yes delm-agent@latest status
npx --yes delm-agent@latest update
npx --yes delm-agent@latest remove
```

Use `--codex PATH` to select an existing CLI, including a relative executable path. Use `--json` for structured output. All native commands run from one temporary directory per invocation, away from project-specific configuration. Existing `CODEX_HOME` selection is respected.

After installation, restart Codex, open `/hooks`, and review and trust the DeLM hooks when requested. Restart again after granting trust, then invoke `$delm:run <task>`. Updates retain Codex's native hook-review requirements. Stop active DeLM work before updating or removing its plugin. Removal retains saved work, accounts, unrelated plugins, and the marketplace registration.

Repeating `install` leaves an enabled plugin in place and reports that no reinstall was needed. Installation status does not verify hook trust in your current session. `status` distinguishes the public plugin from a source installation, and `remove` reports when a source installation remains. Updating a disabled plugin keeps it disabled; use `install` when you want to enable it.

A conflicting `delm` marketplace is preserved and blocks changes. An installed `delm@delm-local` plugin blocks install and update; explicit removal of a verified public plugin remains available and preserves the legacy plugin. An empty `delm-local` marketplace does not block installation. For an ordinary source installation, stop active work, run `./scripts/uninstall.sh` from its original checkout, then retry. That script preserves results and refuses removal of modified cached files. To preserve modified cache contents, use the repository's documented native public-install-then-`migrate.sh` path once a release is available; the npm installer does not automate migration.

Failures are reported with native Codex details. If registration succeeds but installation fails, the registration remains available for inspection and retry. The installer does not automatically remove partial state or unrelated registrations.

## Private verification

From this package directory:

```sh
npm test
npm run test:native
```

The regular tests exercise prerequisites, conflict handling, idempotency, updates, removals, errors, and the package allowlist. Native tests require macOS, Git, npm/npx, and stock Codex CLI. They pack this package, run its actual entrypoint through local `npx`, and redirect the fixed GitHub URL to a disposable local Git fixture using an isolated Git configuration. They install no global packages, use no credentials or model calls, and do not change the user's Codex home. `DELM_TEST_HOST` can select an existing Codex executable for qualification.

To build a private tarball manually, choose an output directory outside this package:

```sh
npm pack --ignore-scripts --pack-destination /absolute/output/directory
```

The tarball contains only the CLI, implementation, package metadata, README, license, and notice. No installation lifecycle scripts run. Publication requires separately reviewing the package name, removing `private: true`, and publishing a qualified native plugin release first.

Mechanics follow [npm executable and package metadata](https://docs.npmjs.com/cli/v11/configuring-npm/package-json), [npm exec/npx arguments](https://docs.npmjs.com/cli/v11/commands/npm-exec), and Codex's native [marketplace](https://github.com/openai/codex/blob/main/codex-rs/cli/src/marketplace_cmd.rs) and [plugin](https://github.com/openai/codex/blob/main/codex-rs/cli/src/plugin_cmd.rs) commands.
