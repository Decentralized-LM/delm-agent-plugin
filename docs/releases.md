# macOS releases

Public releases contain separate self-contained Codex and Claude Code plugins with identical universal macOS runtime bytes. Each package has its native manifest, run skill, lifecycle integration, and license files; Claude also includes its native MCP sidecar configuration and shared worker instructions. Users need Git and their selected host CLI, without a compiler or source checkout. Claude Code 2.1.289 or newer is the supported native API floor. Windows and Linux are separate future work.

The public source repository is [jerry2247/delm-agent-plugin](https://github.com/jerry2247/delm-agent-plugin). The first native marketplace and npm package have not been published. Initial releases will use this repository; a later GitHub transfer is covered below.

## Release destination

The workflow's `release_repository` input accepts `OWNER/REPOSITORY` and defaults to the repository running the workflow. Unsigned review can prepare artifacts for another destination. Publication requires the configured destination to match the workflow repository and its Git remote, so a release cannot accidentally publish to a different origin.

For the first release, use `release_repository: jerry2247/delm-agent-plugin` and `previous_repositories: []`. The latter is a JSON array of approved former GitHub `OWNER/REPO` names; it is only needed after a repository transfer or rename.

The installer source leaves its destination unset and its npm manifest has `private: true`. Release preparation creates the publishable package with the same repository identity as the native plugin. Users cannot override that destination or add former repository addresses when running the installer.

## Prepare a candidate

Run **Prepare macOS release** with `source_ref` set to a branch or full commit SHA and `publish` disabled. The workflow resolves the reference once, checks out that exact clean commit in every job, and retains an unsigned review artifact.

Successful preparation produces `unsigned-macos-review-not-for-distribution` and `prepared-npm-installer-not-published`. The installer artifact contains its tarball, `preparation.json`, and `SHA256SUMS`. Its preparation record binds the configured repository and source revision to the native package metadata. These artifacts let contributors inspect the exact files before publication.

Keep `Cargo.toml`, `Cargo.lock`, `.codex-plugin/plugin.json`, and `hosts/claude/.claude-plugin/plugin.json` on the same version while iterating. The version can remain unchanged until a public release is ready. Do not add a root `plugin.json`: current Codex interprets it as the portable format and skips native lifecycle hooks.

The native plugin is currently **0.3.0**; the npm installer is **0.1.0**. These are separate version sequences. An unchanged installer can install newer native marketplace releases without being republished. Changes to installer behavior, its destination, or approved former repository addresses require a new installer version.

For uncommitted local work, build both host packages and exercise the runtime in disposable directories:

```sh
cargo build --locked --release --bin delm
python3 scripts/build.py --prebuilt target/release/delm --host all
python3 scripts/qualify_release.py smoke --runtime target/release/delm --architecture arm64 --out .validation/release-smoke
```

Use `x86_64` on an actual Intel Mac. Every smoke output directory must be new; previous evidence is preserved. A local universal binary can also be passed to `package_release.py --runtime ... --output ... --repository OWNER/REPOSITORY --revision <base-commit>`. Uncommitted artifacts record `sourceDirty: true`, a runtime source hash, and explicitly identify `sourceRevision` as the base commit. They cannot pass publication qualification. Cross-compilation and Rosetta checks are useful development evidence; Rosetta is explicitly rejected as native Intel qualification.

## Manual Claude candidate qualification

The manually started release workflow uses no Claude account or model calls. It runs native installation fixtures, strict native plugin validation, and a bounded MCP initialize/tools-list probe of the actual bundled runtime. Pushes and pull requests do not start these checks automatically. These establish package compatibility, not successful collaborative task execution.

For release evidence, first prepare an unsigned candidate from a clean source commit. Download that candidate, verify its checksums, and use its exact `plugins/delm-claude` package. From the matching source checkout, run this explicit manual fixture on native Apple Silicon and again on an actual Intel Mac, using each machine's normal authenticated Claude setup:

```sh
python3 scripts/verify_claude_native.py \
  --plugin /path/to/unsigned-review/plugins/delm-claude \
  --out .validation/claude-arm64-candidate \
  --authorize-model-use
```

Use a different new output directory for the Intel run. The fixture launches two native forks for a tiny dependency-free task, requires useful file publications from both workers, checks the delivered combined output, preserves pre-existing files and the Git index, and confirms both worker trees were removed. It changes no Claude settings or permission rules. Raw evidence remains local; only the emitted `qualification.json` is intended for release metadata. Failed or interrupted fixtures produce no passing release record.

The record binds the observed native architecture, exact Claude version, runtime bytes, source inputs, qualification helper, executed adapter resources, and output file hashes. The adapter digest normalizes the manifest's release-supplied repository field; it still binds the adapter's actual executed resources. A debug-binary run is development evidence and cannot qualify a different release binary. A record can match the candidate's unsigned universal binary or its already-qualified native architecture slice. If a rebuild, source edit, helper edit, or adapter change alters those identities, qualify the new candidate before publication. Apple Silicon or Rosetta evidence never establishes native Intel qualification.

The workflow dispatch input `claude_qualifications` accepts a JSON array containing the two sanitized records. Its default `[]` permits unsigned review preparation. To form that array from saved records:

```sh
python3 -c 'import json,sys; print(json.dumps([json.load(open(path)) for path in sys.argv[1:]]))' \
  /path/to/arm64/qualification.json /path/to/x86_64/qualification.json \
  > .validation/claude-qualifications.json
```

Paste that file's contents into the dispatch input. The workflow treats it as data, validates every binding, and runs no authenticated model fixture in CI. Local `package_release.py` accepts the same array through `--claude-qualification FILE`, or one record per repeated flag. `release.json` explicitly lists `claudeQualifiedArchitectures`; missing Intel evidence remains visible. Signing and public publication require both architectures and refuse missing, stale, mismatched, or failed records.

## Qualification and public release

The workflow requires these gates:

| Gate | Required evidence |
| --- | --- |
| Native ARM and Intel builds | Locked dependencies, regular verification, release-profile tests, both native installer fixtures, Claude adapter tests and strict package validation, Codex metadata-fork and saved-configuration inheritance checks, production Codex startup success and failure cases, and all five native Codex lifecycle cases: interrupt, preflight, stop, owner death, plugin removal. |
| Exact release runtimes | Deterministic completion and cancellation with two workers, delivery to the original project, preserved original Git state, durable partial recovery on stop, removal of both temporary workspaces, and no surviving fixture hosts. No model calls. |
| Native Claude task | Matching manual records from both native architectures: two native forks, both workers publish useful files, matching tool pools, preserved original files/index, checked delivered output, and both temporary workspaces removed. Exact runtime, source, adapter, and fixture hashes are bound. |
| Universal assembly | Each extracted architecture slice must match the hash of its tested native binary. Both qualification records identify the same clean source revision and runtime sources. |
| Signing | Developer ID, hardened runtime, timestamp, an accepted Apple notarization submission, and verification of the online notarization record. Plugin resources must match the qualified unsigned package. |
| Signed ARM and Intel execution | The final universal binary runs the same completion/cancellation smoke under quarantine on both native architectures. Reports identify the final signed bytes. |
| Publication | The publisher itself rechecks the expected repository and source revision, clean tagged source, package integrity, both native qualifications, signed smoke reports, and notarization before writing any remote refs. |

Codex native inheritance evidence includes the production metadata-fork request, inherited settings, and an actual DeLM gateway call. Production startup evidence additionally installs the actual plugin in a disposable home and invokes it through the real Codex terminal with a local scripted provider. The success case must deliver its output through two native worker forks and remove their workspaces. The failure case must report the injected provider failure, clean up its run, and allow an ordinary prompt in the same conversation. Each startup case has a 60-second total limit and makes no remote model calls.

The release recorder requires both startup results through `--startup-root`, containing `scripted/result.json` and `failure/result.json`. It binds them to the source, harness, package resources, runtime binary, native architecture, and exact Codex version. Missing, mismatched, failed, or over-budget results cannot qualify a release. Native process cleanup evidence covers the fixture's recorded owned processes. These checks retain the explicit `exactLiveSessionParity: false` limitation described in [support](support.md#codex-setup-and-capability-inheritance); they do not establish support on untested host versions or platforms.

When ready to publish:

1. Create the source tag matching the package version. Protect source tags and `delm-plugin-*` tags against replacement, and protect the `marketplace` branch.
2. Configure the GitHub `release` environment with a required reviewer and these environment secrets: `APPLE_CERTIFICATE_BASE64` (Developer ID Application certificate as a base64-encoded P12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_TEAM_ID`, and `APPLE_APP_PASSWORD`.
3. Obtain matching manual Claude candidate qualification records for native Apple Silicon and Intel as described above.
4. Dispatch **Prepare macOS release** with that tag as `source_ref`, `release_repository: jerry2247/delm-agent-plugin`, `previous_repositories: []`, the records in `claude_qualifications`, and `publish` enabled. The signing job uses the protected environment. After approval, successful signing and both signed native qualification jobs gate the publisher automatically; the publisher receives repository write permission but no Apple credentials.
5. Publish the prepared npm tarball from that successful workflow, following the commands below. Verify installation, update, status, and removal through both hosts before announcing the release.

Failed native checks retain their evidence, and the notarization response is retained when available. Standalone Mach-O binaries cannot carry a stapled notarization ticket; the workflow checks Apple's online record. Never disable Gatekeeper or remove quarantine to bypass a failure.

The build deployment target is macOS 13.0. CI executes on native ARM `macos-15` and native Intel `macos-15-intel`; building with an older deployment target does not establish runtime support on that older system. Qualify the signed package on the oldest advertised system before announcing support. Local development checks do not establish that native Intel CI, signing, notarization, or the GitHub workflow have succeeded. Apple credentials and a protected release environment must be configured separately.

## Publish and install

Publication creates an immutable `delm-plugin-v<version>` tag and advances the `marketplace` branch atomically. The Codex catalog at `.agents/plugins/marketplace.json` points to `plugins/delm` at that immutable tag. Claude uses `.claude-plugin/marketplace.json` and its relative `plugins/delm-claude` package from the same atomically published marketplace tree. Existing package tags cannot be replaced, and concurrent branch changes cause publication to fail rather than overwrite another release. Source files and the source branch remain untouched.

After the native marketplace is published, Codex can also be installed directly:

```sh
codex plugin marketplace add jerry2247/delm-agent-plugin --ref marketplace && codex plugin add delm@delm
```

Restart Codex after installation, open `/hooks`, and review and trust the DeLM hooks. Restart Codex once more, then invoke `$delm:run <task>` in the intended project. Installation does not start workers or grant hook trust.

Updates use `codex plugin marketplace upgrade delm`; review changed hooks in `/hooks` when Codex requests it, then restart before running DeLM. Removal uses `codex plugin remove delm@delm`. Neither requires the source checkout. Native plugin management handles downloading and activation; DeLM adds no updater or background service. Existing `delm-local` users should follow [migration](support.md#updating-and-removing-the-plugin).

Claude installation uses its native manager and the same distribution repository:

```sh
claude plugin marketplace add https://github.com/jerry2247/delm-agent-plugin.git#marketplace --scope user
claude plugin install delm@delm --scope user
```

Restart Claude Code and use `/delm:run <task>`. Update with `claude plugin marketplace update delm`, then `claude plugin update delm@delm --scope user`. Remove with `claude plugin uninstall delm@delm --scope user --keep-data`; saved plugin data remains available. The common installer enforces the supported Claude version before installation or update and preserves conflicting native registrations.

Repository-hosted installation is distinct from listing in OpenAI's public plugin directory. Publication does not imply directory approval.

The `delm-agent` npm installer detects the installed host and delegates marketplace registration and installation to its native CLI. When both hosts are available, it offers Codex, Claude Code, or both. Scripts can choose explicitly with `--host codex`, `--host claude`, or `--host both`. Public installation requires Node.js 22 or later, Git, and the selected host CLI with its login completed.

After a successful workflow with `publish` enabled has published the signed native marketplace, download `prepared-npm-installer-not-published` from that same run. Verify its `SHA256SUMS`, then publish the prepared installer tarball. It already contains the selected repository address, package metadata, and an explicit file allowlist. The source package retains `private: true`; publish the prepared tarball rather than changing that source setting.

```sh
gh run download RUN_ID --name prepared-npm-installer-not-published --dir .validation/installer-publication
cd .validation/installer-publication
shasum -a 256 -c SHA256SUMS
npm login
npm publish ./delm-agent-0.1.0.tgz --access public
```

Run these commands from this repository's checkout. Replace `RUN_ID` with the successful publication workflow's run ID and use the filename produced for the installer version being released. Registry account ownership and authentication are required. Each published installer version is immutable; increment its package version when its code or destination changes. Users then install with:

```sh
npx --yes delm-agent@latest install
```

The destination repository must be public for installation without GitHub access credentials. Before announcing a release, verify its installation using the published command, followed by native hook review, update, and removal. The final native plugin and installer must identify the same repository. Until both are published, the command above is not an available public installation path.

## Transfer the repository later

Use GitHub's repository transfer operation to move this same repository. GitHub redirects existing Git clone, fetch, and push URLs to the new location. Creating a copy in another repository does not transfer those URLs. Do not create a new repository or fork at `jerry2247/delm-agent-plugin` after the transfer: GitHub says that reusing the old location permanently removes its redirects. See [GitHub's transfer documentation](https://docs.github.com/en/repositories/creating-and-managing-repositories/transferring-a-repository).

After the transfer:

1. Update the maintainer checkout's `origin` and public documentation to the new repository. Confirm its release environment, workflow permissions, and release protections.
2. Keep existing plugin tags unchanged. Publish the next qualified native release from the new home when updating its distribution metadata; an existing `delm-plugin-v<version>` tag cannot be replaced.
3. Increment `packages/installer/package.json`'s installer version. Prepare it with the new `release_repository` and `previous_repositories: ["jerry2247/delm-agent-plugin"]`. Keep any additional approved former addresses in that array. Preparation updates the npm repository, homepage, issue links, and installation configuration.
4. Publish that prepared installer tarball, then verify a fresh installation and an update from an installation registered at the old address for each host.

For local release preparation, repeat `--previous-repository` for each approved former address:

```sh
python3 scripts/prepare_installer.py \
  --repository NEW_OWNER/NEW_REPOSITORY \
  --previous-repository jerry2247/delm-agent-plugin \
  --native-release /path/to/new-release/release.json \
  --out .validation/prepared-installer-transfer
```

The installer accepts only the current repository and the explicitly approved former addresses embedded in its release configuration. It keeps an existing native marketplace registration at its approved old URL and lets the host update through GitHub's redirect. It does not silently replace that registration. Fresh installations use the new URL; unrelated registrations remain conflicts. Test the redirect before publishing and preserve it afterward. The npm package name and user command remain `delm-agent` and `npx --yes delm-agent@latest install`.

## Package verification

The release artifact includes schema-1 `release.json` and `SHA256SUMS`. The manifest records source revision, repository, version, architectures, signing status, working-tree status, runtime source hash, Codex native qualification records, actual Claude qualification architecture coverage, Claude adapter and fixture hashes, native static validation, and each host package file's checksum and permissions. A signed package also identifies its qualified unsigned runtime. Verify an extracted artifact with:

```sh
python3 scripts/package_release.py --verify /path/to/extracted-release
```

For CI provenance checks, supply `--revision <expected-commit> --repository OWNER/REPOSITORY --require-qualified`. Signing additionally rechecks the original unsigned slices. Publication requires the two final signed smoke reports through `publish_release.py PACKAGE SIGNED_QUALIFICATION_REPORTS`; the basic checksum check alone does not authorize publishing. Checksums detect changes against the artifact's manifest; signing and a trusted workflow/repository establish the source.

References: [Claude native plugin packaging](https://code.claude.com/docs/en/plugins/manifest-reference), [Claude plugin commands](https://code.claude.com/docs/en/plugins/cli-reference), [workflow input syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#onworkflow_dispatchinputs), [native plugin packaging](https://developers.openai.com/plugins/build/plugins), [GitHub-hosted runner architectures](https://docs.github.com/en/actions/reference/runners/github-hosted-runners), [Apple distribution signing](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/), and [notarization workflows](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).
