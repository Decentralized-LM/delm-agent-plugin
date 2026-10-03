# DeLM installer

This source package is **unpublished**, has not reserved its npm name, and has `private: true` to prevent accidental publication. Its `release.json` deliberately has no repository configured. Management commands refuse to run until a release package is prepared; help and version remain available. The signed native plugin marketplace has not been published either. The command below is the planned public interface; it is not an available install command yet:

```sh
npx --yes delm-agent@latest install
```

The installer is a small, dependency-free Node CLI. It delegates installation, updates, removal, and status to stock Codex's native plugin manager. A prepared package installs `delm@delm` from its configured repository's `marketplace` branch. Preparation takes one GitHub `OWNER/REPO` destination and fixes it inside that package; users cannot override it with a CLI flag. The installer does not replace Codex, edit configuration files itself, grant hook trust, start workers, or run an updater. The installer version is independent of the plugin version; a new plugin release does not require publishing a new installer.

## Requirements and commands

Installation currently supports macOS. Node.js 22+, stock Codex CLI, and Git are required. A desktop or IDE installation without Codex CLI is insufficient. Complete account login through Codex before running DeLM; this installer does not inspect credentials. In a prepared package, `--help`, `--version`, and read-only `status` are available on other operating systems. Users need Git access to its distribution repository; publishing npm does not make a private Git repository publicly accessible.

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

Private tests also require Python 3 and npm. The regular tests exercise prerequisites, conflict handling, idempotency, updates, removals, errors, preparation, the package allowlist, and an offline npm publication dry run. Native tests additionally require macOS, Git, npm/npx, and stock Codex CLI. They prepare a package for a test repository, run its generated tarball through local `npx`, and redirect that GitHub URL to a disposable local Git fixture using an isolated Git configuration. They install no global packages, use no credentials or model calls, and do not change the user's Codex home. `DELM_TEST_HOST` can select an existing Codex executable for qualification.

To prepare a release installer, run this from the repository root with the intended distribution destination and a fresh output directory:

```sh
python3 scripts/prepare_installer.py --repository OWNER/REPO --out .validation/prepared-installer
```

Optionally add `--native-release /path/to/native/release.json` to require the same repository and bind that native release's metadata hash and source provenance in `preparation.json`. Preparation does not qualify or publish a native release. The installer version comes from this package's `package.json`, independently of the native version.

The output contains `package/`, `delm-agent-<version>.tgz`, `preparation.json`, and `SHA256SUMS`. The generated package has the chosen repository, public-use help and README, and `private: false`; the source files remain unchanged and protected. The checksum file covers the tarball and preparation record so it can be verified with those files alone. The preparation record also hashes each packaged file. Reusing an output directory is refused.

The tarball contains only the CLI, implementation, release configuration, package metadata, README, license, and notice. Packing uses isolated npm configuration and no lifecycle scripts or registry access. Publish the generated tarball only after separately confirming the npm name and publishing a qualified, accessible native plugin release. Do not remove the source package's publication guard.

Mechanics follow [npm executable and package metadata](https://docs.npmjs.com/cli/v11/configuring-npm/package-json), [npm exec/npx arguments](https://docs.npmjs.com/cli/v11/commands/npm-exec), and Codex's native [marketplace](https://github.com/openai/codex/blob/main/codex-rs/cli/src/marketplace_cmd.rs) and [plugin](https://github.com/openai/codex/blob/main/codex-rs/cli/src/plugin_cmd.rs) commands.
