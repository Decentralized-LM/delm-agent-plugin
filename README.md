<p align="center">
  <a href="https://yuzhenmao.github.io/DeLM/">
    <img src="docs/assets/delm-wordmark.svg" alt="DeLM" width="240">
  </a>
</p>

<h1 align="center">DeLM for Codex and Claude Code</h1>
<p align="center">Build faster with agents that work together.</p>

<p align="center">
  <a href="https://arxiv.org/abs/2606.10662"><img src="docs/assets/paper.svg" alt="Read the paper on arXiv" height="28"></a>
  &nbsp;
  <a href="https://yuzhenmao.github.io/DeLM/"><img src="docs/assets/website.svg" alt="Visit the project website" height="28"></a>
  &nbsp;
  <a href="https://discord.com/invite/EuQyJPJzBt"><img src="docs/assets/discord.svg" alt="Join the DeLM community on Discord" height="28"></a>
</p>

<p align="center">
  <a href="#install-on-macos">Install</a> ·
  <a href="#run-a-task">Usage</a> ·
  <a href="docs/support.md">Support</a> ·
  <a href="CONTRIBUTING.md">Contributing</a>
</p>

DeLM lets agents work in parallel in your existing Codex or Claude Code workflow. A shared task queue coordinates the work, while shared context lets agents exchange findings and reuse each other's code. Their contributions come together in your project.

- **Work in parallel.** Agents claim tasks and pick up new work as it becomes available.
- **Share progress.** Findings, files, and relevant checks are available to the other agents as they work.
- **Keep your workflow.** Start a run in your usual conversation, send clarifications there, and receive the changes in your original project.

DeLM uses your existing host account and starts agents only when you ask.

## Install on macOS

**Release pending.** Public installation will use one command for either or both plugins:

```sh
npx --yes delm-agent@latest install
```

Requires macOS, Node.js 22 or later, Git, and the CLI for your chosen host with its login completed. Claude Code requires version **2.1.289 or later**.

The installer detects your installed host. If both Codex and Claude Code are available, choose **Codex**, **Claude Code**, or **Both**. Explicit host flags are available for [scripted installation](packages/installer/README.md#host-selection). Each plugin uses its host's native plugin manager.

| Host | After installation |
| --- | --- |
| Codex | Restart, open `/hooks`, and review and trust the DeLM hooks. Restart once more to load them. |
| Claude Code | Restart to load DeLM. Review any trust or permission prompts Claude presents. |

See [support](docs/support.md) for updates, removal, and troubleshooting, or the [release guide](docs/releases.md) for publication requirements.

## Run a task

Open your host in the project you want to change, then invoke DeLM with a request:

| Host | Start a run |
| --- | --- |
| Codex | `$delm:run <your task>` |
| Claude Code | `/delm:run <your task>` |

For example, in Claude Code:

```text
/delm:run Build a task board with drag-and-drop columns, local persistence,
and keyboard controls. Include a README and test the main interactions.
```

Use `$delm:run` for the same request in Codex. Send clarifications in the same conversation while the agents work.

In Claude Code, a live board opens automatically in the same terminal. It shows the agents, task queue, and shared context while you keep using the normal prompt. Click a row for details. **Hide board** leaves work running; `/delm-status` reopens it without asking the model for a summary. Use `/delm-stop` to stop the run and save unfinished changes. If a run needs recovery, normal Claude conversation remains available and the board shows the next action. See [run control and recovery](docs/support.md) for details.

Include images and file mentions when starting a Claude run. During a run, send text or paste the relevant file content. Claude's current native API cannot forward new media attachments to existing agents; DeLM explains this before accepting an unsupported update. See [Claude input support](docs/claude-integration.md) for details.

The current version runs two agents in private project copies. They share contributions and divide useful checks, so a recorded check can be reused when it still applies to the result.

**The result is delivered to your original project.** DeLM applies source changes and requested artifacts, preserves your Git index, merges compatible edits, and retains conflicts for recovery. Saved partial changes can be exported into a new folder for review. Temporary worker directories are removed after safe delivery or recovery. When the delivered project needs dependency setup or a focused check, the parent completes it before reporting the result ready.

Choose one project smaller than 10 GB, including ignored files and Git history. DeLM initializes Git in that folder if needed without creating a commit. Runs have a 30-minute default allowance.

### Your host setup

Claude's native forks inherit the current conversation, model, system prompt, and available tools. Coordination tools pass through Claude's normal permission checks.

Codex workers preserve saved skills, plugins, hooks, MCP configuration, and permissions. Codex's current fork API does not expose all parent-process CLI overrides, so exact live-session parity is still a limitation. The [support guide](docs/support.md#codex-setup-and-capability-inheritance) explains what is inherited and how to check your setup.

## Documentation

| Guide | Contents |
| --- | --- |
| [Support](docs/support.md) | Requirements, permissions, timing reports, updates, and recovery |
| [Architecture](docs/architecture.md) | Host adapters, shared coordination, and project delivery |
| [Contributing](CONTRIBUTING.md) | Development setup and verification |
| [Release guide](docs/releases.md) | Build qualification and distribution |
| [Claude integration](docs/claude-integration.md) | Native integration design and validation |

## Research

DeLM builds on **Decentralized Multi-Agent Systems with Shared Context**. See the [paper](https://arxiv.org/abs/2606.10662), [project website](https://yuzhenmao.github.io/DeLM/), and [research code](https://github.com/yuzhenmao/DeLM) for the method, evaluations, and agent trajectories.

The Codex and Claude Code plugins were created by [Jerry Gu](https://github.com/jerry2247).

## License

DeLM is licensed under [MIT](LICENSE). Third-party components retain their own licenses; see [NOTICE](NOTICE).
