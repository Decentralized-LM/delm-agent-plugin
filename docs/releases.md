# macOS releases

Public releases contain a universal macOS binary, the native Codex plugin manifest, the explicit `delm:run` skill, lifecycle hooks, and license files. Users need Codex CLI and Git, without a compiler or a source checkout. Windows and Linux are separate future work.

The source repository currently has no published marketplace. The workflow derives installation URLs from its actual GitHub repository, so no owner or public URL is hard-coded here.

## Prepare a release

1. Keep `Cargo.toml`, `Cargo.lock`, and `.codex-plugin/plugin.json` on the same version. Complete the regular checks and native sandbox checks on the supported architectures. Do not add a root `plugin.json` to the package: current Codex treats that as the portable format and skips lifecycle hooks.
2. Create a source tag matching that version, such as `v0.3.0`. Protect source tags and `delm-plugin-*` tags against replacement, and protect the `marketplace` branch.
3. Run **Prepare macOS release**, selecting the source tag and leaving `publish` disabled. The workflow resolves the tag once and passes its exact commit to every job. Each architecture builds and tests with locked dependencies, then assembly produces a universal binary.
4. Inspect the unsigned review artifact. Check the generated README, package contents, architecture slices, version, source revision, and checksums. This artifact is not a public installer.
5. Configure the protected GitHub `release` environment with a required reviewer. Supply `APPLE_CERTIFICATE_BASE64` (Developer ID Application certificate as a base64-encoded P12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_TEAM_ID`, and `APPLE_APP_PASSWORD` as environment secrets.
6. Dispatch the same workflow with `publish` enabled. The protected job signs with hardened runtime and a timestamp, submits for notarization, verifies Apple's notarization record, and runs a quarantined copy without removing its quarantine attribute. Any failure prevents publication.

The build deployment target is macOS 13.0. CI executes on macOS 15; building with an older deployment target does not establish runtime support on that older system. Qualify the signed package on the oldest advertised system before announcing support. Local development checks do not establish that Intel builds, signing, notarization, or the GitHub workflow have succeeded.

## Publish and install

Publication creates an immutable `delm-plugin-v<version>` tag and advances the `marketplace` branch atomically. The marketplace catalog points to `plugins/delm` at that immutable tag. Existing package tags cannot be replaced, and concurrent branch changes cause publication to fail rather than overwrite another release. Source files and the source branch remain untouched.

The generated release README supplies the exact one-paste command using the real repository name:

```sh
codex plugin marketplace add OWNER/REPOSITORY --ref marketplace && codex plugin add delm@delm
```

`OWNER/REPOSITORY` above describes the command format, not a published destination. Copy the fully resolved command from the published package README. Restart Codex after installation, open `/hooks`, and review and trust the DeLM hooks. Restart Codex once more, then invoke `$delm:run <task>` in a Git repository. Installation does not start workers or grant hook trust.

Updates use `codex plugin marketplace upgrade delm`; review changed hooks in `/hooks` when Codex requests it, then restart before running DeLM. Removal uses `codex plugin remove delm@delm`. Neither requires the source checkout. Native plugin management handles downloading and activation; DeLM adds no updater or background service. Existing `delm-local` users should follow [migration](support.md#updating-and-removing-the-plugin).

Repository-hosted installation is distinct from listing in OpenAI's public plugin directory. Publication does not imply directory approval.

## Package verification

The release artifact includes `release.json` and `SHA256SUMS`. The former records source revision, version, architectures, signing status, and each runtime file's checksum and permissions. Verify an extracted artifact with:

```sh
python3 scripts/package_release.py --verify /path/to/extracted-release
```

Checksums detect changes against the artifact's manifest; signing and a trusted repository establish the source. Do not suggest disabling Gatekeeper or removing quarantine to bypass a release failure.

References: [native plugin packaging](https://developers.openai.com/plugins/build/plugins), [Apple distribution signing](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/), and [notarization workflows](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).
