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

DeLM brings two collaborating agents to your existing Codex conversation. They claim work, share findings, and reuse each other's code through a shared context and task queue. Each agent develops a complete solution in a private workspace; the first to finish returns the result.

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

Open Codex in the Git repository you want to work on and enter a request:

```text
$delm:run Build a task board with drag-and-drop columns, local persistence,
and keyboard controls. Include a README and test the main interactions.
```

Both agents can research, install dependencies, run tests, and check a private browser preview. You can clarify the request or ask Codex to stop from the same conversation.

**Your original repository stays unchanged.** DeLM returns a link to the completed project, launch instructions, a review of the changes, and the checks performed. Open that project to continue, or ask Codex to apply the changes after review. Interrupted work is also preserved.

Use a standalone Git repository smaller than 10 GB, including ignored files and Git history. Runs have a 30-minute default allowance. Stop an active run before disabling DeLM or revoking its hook trust.

## Learn more

- [Support](docs/support.md) covers project requirements, permissions, updates, and recovery.
- [Architecture](docs/architecture.md) explains workspaces, coordination, and completion.
- [Contributing](CONTRIBUTING.md) covers development and checks.
- [Research code](https://github.com/yuzhenmao/DeLM) contains the paper's evaluation framework and results.

Licensed under [Apache-2.0](LICENSE).
