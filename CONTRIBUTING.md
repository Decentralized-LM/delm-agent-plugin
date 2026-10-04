# Contributing

DeLM supports native Codex and Claude Code workflows on macOS. Both host adapters share the collaboration policy, task board, workspace handling, and delivery implementation. Follow [development setup](docs/development.md) for prerequisites, local host packages, and verification.

Keep changes focused and explain the problem, resulting behavior, and checks you ran. Changes to worker prompts or coordination should state how they affect the two-worker DeLM mechanism. Include a regression test when a change fixes behavior that could recur.

Before submitting, run:

```sh
./scripts/verify.sh
```

Changes to installation, capability inheritance, or process ownership also need the relevant [host-specific checks](docs/development.md#focused-verification). Native installer checks for both hosts and Codex inheritance and lifecycle fixtures use disposable configurations without model calls. The Claude real-task fixture is separate, requires explicit account-use authorization, and stays outside routine CI. Report which checks passed and any you could not run; saved-configuration checks alone do not prove exact live-session parity, and a local fixture does not qualify a public release.

Keep credentials, private run folders, local builds, and research notes out of contributions. Bug reports should include the DeLM version, host name and version, macOS version and architecture, installation method, exact error, and a small reproduction. For permission or lifecycle failures, include the native permission mode and the operation that failed. Share only the logs needed to explain the issue, with private content removed.

See [architecture](docs/architecture.md) for runtime behavior and [releases](docs/releases.md) for packaging and publication.
