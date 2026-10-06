# Contributing to DeLM

We welcome bug fixes, documentation improvements, and ideas for making DeLM better in Codex and Claude Code.

## Quick links

- [GitHub](https://github.com/Decentralized-LM/delm-agent-plugin)
- [Discord](https://discord.com/invite/EuQyJPJzBt)
- [Project website](https://yuzhenmao.github.io/DeLM/)
- [Research paper](https://arxiv.org/abs/2606.10662)

## How to contribute

- **Bugs and small fixes:** open an issue or a pull request. Link any related discussion.
- **Features and larger changes:** start a [GitHub issue](https://github.com/Decentralized-LM/delm-agent-plugin/issues) or discuss the idea on [Discord](https://discord.com/invite/EuQyJPJzBt) before implementing it.
- **Questions and setup help:** ask on Discord or check the [support guide](docs/support.md).

## Install from source

Development requires macOS, Git, Python 3, Node.js 22 or later, Xcode Command Line Tools, and the Rust toolchain specified in [rust-toolchain.toml](rust-toolchain.toml). Install the CLI for the host you want to work on: Codex or Claude Code 2.1.289 or later.

```sh
git clone https://github.com/Decentralized-LM/delm-agent-plugin.git
cd delm-agent-plugin
```

Follow the development guide to [set up Codex](docs/development.md#codex-plugin) or [set up Claude Code](docs/development.md#claude-code-plugin). You can install both. Sign in through your chosen host before trying an interactive run; routine verification does not need an account.

If you already have DeLM installed, review [source installation maintenance](docs/support.md#retained-state-and-contributor-installations) before switching installations or rebuilding.

## Before opening a pull request

Keep each PR focused on one problem. Explain what changes for the user and how you checked it. For changes to worker prompts or coordination, describe how the agents' behavior changes. Include a regression test for a bug fix and screenshots for visual changes when useful.

For code changes, run:

```sh
./scripts/verify.sh
```

Installation, capability inheritance, and process lifecycle changes also need the relevant [host-specific checks](docs/development.md#focused-verification). Run checks locally and report what passed or could not be tested; pushes and pull requests do not start automatic verification workflows. [Real-task checks](docs/development.md#optional-real-task-qualification) use a host account and require its owner's authorization.

For documentation-only changes, check the wording, links, and commands you changed. Keep credentials, private run logs, local builds, and research notes out of your PR.

## Reporting bugs

Include enough detail to reproduce the problem:

- What you expected and what happened, including the exact error.
- A small reproduction or the steps that led to the failure.
- DeLM and host versions, installation method, macOS version, and Apple Silicon or Intel.
- Relevant logs or screenshots, with private content removed.

For permission or process lifecycle issues, also include the host's permission mode and the operation that failed.

See the [development guide](docs/development.md) for build and test details, [architecture](docs/architecture.md) for how DeLM works, and [release guide](docs/releases.md) for packaging and publication.
