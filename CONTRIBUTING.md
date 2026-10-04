# Contributing

DeLM is developed on macOS with stock Codex CLI. Follow [development setup](docs/development.md) for prerequisites, local installation, and native checks. The public plugin release is not available yet.

Keep changes focused and explain the problem, resulting behavior, and checks you ran. Changes to worker prompts or coordination should state how they affect the two-worker DeLM mechanism. Include a regression test when a change fixes behavior that could recur.

Before submitting, run:

```sh
./scripts/verify.sh
```

Changes to capability inheritance or process ownership also need the relevant [native inheritance checks](docs/development.md) or [lifecycle checks](docs/native-lifecycle-qualification.md). These use disposable fixtures without model calls. Report which checks passed and any you could not run; saved-configuration checks alone do not prove exact live-session parity.

Keep credentials, private run folders, local builds, and research notes out of contributions. Bug reports should include the DeLM and Codex versions, macOS version, exact error, and a small reproduction. Share only the logs needed to explain the issue, with private content removed.

See [architecture](docs/architecture.md) for runtime behavior and [releases](docs/releases.md) for packaging and publication.
