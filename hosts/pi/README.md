# DeLM for Pi

Build in parallel with collaborating [Pi](https://pi.dev) agents and deliver their changes to your project.

Install a published release with the common installer or directly with Pi:

```sh
npx --yes delm-agent@latest install --host pi
# or
pi install git:github.com/Decentralized-LM/delm-agent-plugin@marketplace
```

Then start a new Pi session and run `/delm:run <task>`. Requires macOS, Pi 1.4.0 or later, Git, and a Pi login.

For development, this directory is installed as a local Pi package. Its `bin/delm` runtime, the shared worker policy
(`worker.md`, staged from the repository's `plugin/worker.md`) and the two extensions are staged by the repository's build script:

```sh
./scripts/build.sh --host pi
pi install "$PWD/.build/plugin-pi"
```

Then, in the project you want to change:

```
/delm:run <your task>
```

DeLM prepares two private copies of the project, forks your conversation into two Pi workers,
coordinates them through a shared task board, and delivers the accepted result back into your
project. Text you send while the run is working is forwarded to both workers. `/delm:status` shows
the run; `/delm:stop` cancels it and saves unfinished changes.

Design and limitations: [docs/pi-integration.md](../../docs/pi-integration.md).
