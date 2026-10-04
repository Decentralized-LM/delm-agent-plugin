<p align="center">
  <a href="https://yuzhenmao.github.io/DeLM/">
    <img src="docs/assets/delm-wordmark.svg" alt="DeLM" width="240">
  </a>
</p>

<h1 align="center">DeLM for Codex</h1>
<p align="center">Decentralized collaboration in your Codex workflow.</p>

<p align="center">
  <a href="https://arxiv.org/abs/2606.10662"><img src="docs/assets/paper.svg" alt="Read the paper on arXiv" height="28"></a>
  &nbsp;
  <a href="https://yuzhenmao.github.io/DeLM/"><img src="docs/assets/website.svg" alt="Visit the project website" height="28"></a>
  &nbsp;
  <a href="https://discord.com"><img src="docs/assets/discord.svg" alt="Discord" height="28"></a>
</p>

<p align="center">
  <a href="#install-on-macos">Install</a> ·
  <a href="#run-a-task">Usage</a> ·
  <a href="docs/support.md">Support</a> ·
  <a href="CONTRIBUTING.md">Contributing</a>
</p>

DeLM brings two collaborating agents to your existing Codex conversation. They claim work, share findings, and reuse each other's code through a shared context and task queue. Both contribute to one result, which DeLM applies to your project before removing its temporary workspaces.

Invoke **`$delm:run`** when you want them to work together. DeLM uses your Codex account and starts its workers only when you ask.

<p align="center">
  <a href="video-demo/renders/delm-demo.mp4"><img src="video-demo/renders/poster.png" alt="Watch DeLM for Codex: two agents building and sharing their work" width="800"></a>
  <br>
  <a href="video-demo/renders/delm-demo.mp4">Watch the 56-second demo</a>
</p>

## Install on macOS

The plugin currently installs from source through Codex's native plugin manager. Prebuilt releases have not yet been published.

You'll need Codex CLI with an existing login, Git, Python 3, Rust, and Xcode Command Line Tools. From this repository's root, run:

```sh
./scripts/install.sh
```

Restart Codex, open `/hooks`, and review and trust the DeLM hooks. Restart once more to load the configuration. Installation leaves your Codex executable and account credentials unchanged. See [development setup](docs/development.md) for build details.

The planned npm command is `npx --yes delm-agent@latest install`. The `delm-agent` package is not published or reserved yet; use the source installation above. Contributors can [test the installer privately](packages/installer/README.md).

## Run a task

Open Codex in the project you want to work on and enter a request:

```text
$delm:run Build a task board with drag-and-drop columns, local persistence,
and keyboard controls. Include a README and test the main interactions.
```

The trusted invocation hook starts the runtime directly. Both agents work in private project copies, share contributions, and take new tasks as work becomes available. Independent checks can run in parallel; applicable recorded checks can be reused instead of repeating a full acceptance pass. You can clarify the request or ask Codex to stop from the same conversation.

**Changes are delivered to your original project.** DeLM preserves the Git index, merges compatible concurrent edits, and saves conflicting changes for recovery. Both worker directories are removed after safe delivery or recovery. Dependency environments stay local to each project; when delivery changes dependency manifests, omits worker-local environments, or merges your edits, Codex performs the necessary setup or focused check in the original project before reporting it ready.

Workers use native Codex forks and preserve ordinary saved skills, plugins, hooks, MCP configuration, and permissions instead of disabling them. The current host API omits parent-process CLI overrides when creating a separate fork host, so this development build does not yet provide exact live-session parity. See [capability inheritance](docs/support.md#codex-setup-and-capability-inheritance) for the precise boundary.

Select one project smaller than 10 GB, including ignored files and Git history. DeLM initializes Git in that exact folder if needed; it does not create a commit. Runs have a 30-minute default allowance. Stop an active run before disabling DeLM or revoking its hook trust.

## Learn more

- [Support](docs/support.md) covers project requirements, permissions, updates, and recovery.
- [Architecture](docs/architecture.md) explains workspaces, coordination, and completion.
- [Contributing](CONTRIBUTING.md) covers development and checks.
- [Research code](https://github.com/yuzhenmao/DeLM) contains the paper's evaluation framework and results.

Licensed under [Apache-2.0](LICENSE).
