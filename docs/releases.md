# macOS releases

Public releases contain a universal macOS binary, the native Codex plugin manifest, the explicit `delm:run` skill, lifecycle hooks, and license files. Users need Codex CLI and Git, without a compiler or a source checkout. Windows and Linux are separate future work.

The source repository currently has no published marketplace. The release workflow resolves one repository address and uses it for the native marketplace, plugin metadata, and prepared installer. A new public home does not require changing the installer implementation.

## Release destination

The workflow's `release_repository` input accepts `OWNER/REPOSITORY` and defaults to the repository running the workflow. Unsigned review can prepare artifacts for another destination. Publication requires the configured destination to match the workflow repository and its Git remote, so a release cannot accidentally publish to a different origin.

The installer source has no configured destination and remains private. Release preparation creates a separate configured package with the same repository identity as the native plugin. The generated package is what users receive; its repository cannot be overridden when they run it.

## Private iteration

Run **Prepare macOS release** with `source_ref` set to a branch or full commit SHA and `publish` disabled. The workflow resolves the reference once, checks out that exact clean commit in every job, and retains an unsigned review artifact. A private repository keeps its workflow artifacts private to those with access.

Successful preparation produces `unsigned-macos-review-not-for-distribution` and `prepared-npm-installer-not-published`. The installer artifact contains its tarball, `preparation.json`, and `SHA256SUMS`. Its preparation record binds the configured repository and source revision to the native package metadata. These artifacts let contributors inspect the exact files before publication.

Keep `Cargo.toml`, `Cargo.lock`, and `.codex-plugin/plugin.json` on the same version while iterating. The version can remain unchanged until a public release is ready. Do not add a root `plugin.json`: current Codex interprets it as the portable format and skips native lifecycle hooks.

For uncommitted local work, build and exercise the runtime in disposable directories:

```sh
cargo build --locked --release --bin delm
python3 scripts/qualify_release.py smoke --runtime target/release/delm --architecture arm64 --out .validation/release-smoke
```

Use `x86_64` on an actual Intel Mac. Every smoke output directory must be new; previous evidence is preserved. A local universal binary can also be passed to `package_release.py --runtime ... --output ... --repository OWNER/REPOSITORY --revision <base-commit>`. Uncommitted artifacts record `sourceDirty: true`, a runtime source hash, and explicitly identify `sourceRevision` as the base commit. They cannot pass publication qualification. Cross-compilation and Rosetta checks are useful development evidence; Rosetta is explicitly rejected as native Intel qualification.

## Qualification and public release

The workflow requires these gates:

| Gate | Required evidence |
| --- | --- |
| Native ARM and Intel builds | Locked dependencies, regular verification, release-profile tests, native sandbox test, and all five native lifecycle cases: interrupt, preflight, stop, owner death, plugin removal. |
| Exact release runtimes | Deterministic completion and cancellation with two workers, retained output, unchanged original project, and no surviving fixture hosts. No model calls. |
| Universal assembly | Each extracted architecture slice must match the hash of its tested native binary. Both qualification records identify the same clean source revision and runtime sources. |
| Signing | Developer ID, hardened runtime, timestamp, an accepted Apple notarization submission, and verification of the online notarization record. Plugin resources must match the qualified unsigned package. |
| Signed ARM and Intel execution | The final universal binary runs the same completion/cancellation smoke under quarantine on both native architectures. Reports identify the final signed bytes. |
| Publication | The publisher itself rechecks the expected repository and source revision, clean tagged source, package integrity, both native qualifications, signed smoke reports, and notarization before writing any remote refs. |

When ready to publish:

1. Create the matching source tag, such as `v0.3.0`. Protect source tags and `delm-plugin-*` tags against replacement, and protect the `marketplace` branch.
2. Configure the GitHub `release` environment with a required reviewer and these environment secrets: `APPLE_CERTIFICATE_BASE64` (Developer ID Application certificate as a base64-encoded P12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_TEAM_ID`, and `APPLE_APP_PASSWORD`.
3. Dispatch **Prepare macOS release** with that tag as `source_ref` and `publish` enabled. The signing job uses the protected environment. After approval, successful signing and both signed native qualification jobs gate the publisher automatically; the publisher receives repository write permission but no Apple credentials.

Failed native checks retain their evidence, and the notarization response is retained when available. Standalone Mach-O binaries cannot carry a stapled notarization ticket; the workflow checks Apple's online record. Never disable Gatekeeper or remove quarantine to bypass a failure.

The build deployment target is macOS 13.0. CI executes on native ARM `macos-15` and native Intel `macos-15-intel`; building with an older deployment target does not establish runtime support on that older system. Qualify the signed package on the oldest advertised system before announcing support. Local development checks do not establish that native Intel CI, signing, notarization, or the GitHub workflow have succeeded. Apple credentials and a protected release environment must be configured separately.

## Publish and install

Publication creates an immutable `delm-plugin-v<version>` tag and advances the `marketplace` branch atomically. The marketplace catalog points to `plugins/delm` at that immutable tag. Existing package tags cannot be replaced, and concurrent branch changes cause publication to fail rather than overwrite another release. Source files and the source branch remain untouched.

The generated release README supplies the exact one-paste command using the real repository name:

```sh
codex plugin marketplace add OWNER/REPOSITORY --ref marketplace && codex plugin add delm@delm
```

`OWNER/REPOSITORY` above describes the command format, not a published destination. Copy the fully resolved command from the published package README. Restart Codex after installation, open `/hooks`, and review and trust the DeLM hooks. Restart Codex once more, then invoke `$delm:run <task>` in a Git repository. Installation does not start workers or grant hook trust.

Updates use `codex plugin marketplace upgrade delm`; review changed hooks in `/hooks` when Codex requests it, then restart before running DeLM. Removal uses `codex plugin remove delm@delm`. Neither requires the source checkout. Native plugin management handles downloading and activation; DeLM adds no updater or background service. Existing `delm-local` users should follow [migration](support.md#updating-and-removing-the-plugin).

Repository-hosted installation is distinct from listing in OpenAI's public plugin directory. Publication does not imply directory approval.

The npm wrapper in `packages/installer` delegates marketplace registration and installation to the stock Codex CLI. Its proposed name is `delm-agent`, which remains unpublished and unreserved. Its version is independent of the plugin version, so a new plugin release does not require republishing an unchanged installer.

After a successful workflow with `publish` enabled has published the signed native marketplace, download `prepared-npm-installer-not-published` from that same run. Verify its `SHA256SUMS`, then publish the prepared installer tarball. It already contains the selected repository address, package metadata, and an explicit file allowlist. The source package retains `private: true`; publish the prepared tarball rather than changing that source setting.

```sh
gh run download RUN_ID --name prepared-npm-installer-not-published --dir .validation/installer-publication
cd .validation/installer-publication
shasum -a 256 -c SHA256SUMS
npm login
npm publish ./delm-agent-0.1.0.tgz --access public
```

Run these commands from the final repository's checkout. Replace `RUN_ID` with the successful publication workflow's run ID and use the filename produced for the installer version being released. Registry account ownership and authentication are required. Each published installer version is immutable; increment its package version when its code or destination changes. Users then install with:

```sh
npx --yes delm-agent@latest install
```

The destination repository must be public for installation without GitHub access credentials. Before announcing a release, verify its installation using the published command, followed by native hook review, update, and removal. The final native plugin and installer must identify the same repository. Until both are published, the command above is not an available public installation path.

## Package verification

The release artifact includes schema-1 `release.json` and `SHA256SUMS`. The manifest records source revision, repository, version, architectures, signing status, working-tree status, runtime source hash, native qualification records, and each plugin file's checksum and permissions. A signed package also identifies its qualified unsigned runtime. Verify an extracted artifact with:

```sh
python3 scripts/package_release.py --verify /path/to/extracted-release
```

For CI provenance checks, supply `--revision <expected-commit> --repository OWNER/REPOSITORY --require-qualified`. Signing additionally rechecks the original unsigned slices. Publication requires the two final signed smoke reports through `publish_release.py PACKAGE SIGNED_QUALIFICATION_REPORTS`; the basic checksum check alone does not authorize publishing. Checksums detect changes against the artifact's manifest; signing and a trusted workflow/repository establish the source.

References: [native plugin packaging](https://developers.openai.com/plugins/build/plugins), [GitHub-hosted runner architectures](https://docs.github.com/en/actions/reference/runners/github-hosted-runners), [Apple distribution signing](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/), and [notarization workflows](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).
