# Native lifecycle qualification

The reusable test uses stock Codex, a disposable `HOME` and `CODEX_HOME`, a local hardcoded Responses API stream, and a harmless Rust fixture that imports DeLM's actual lifecycle module. It does not run a real model, load existing credentials, modify an installed plugin, or change a user UI session. Native hook trust is simulated explicitly inside the disposable home, after confirming that installation alone leaves all hooks untrusted.

Build and run from the repository root on macOS with stock `codex` on `PATH`. Each output directory must be new. Loopback listening and launching the installed CLI require an execution environment that permits those operations.

```sh
cargo build --offline --example native-lifecycle-fixture
python3 scripts/verify_native_lifecycle.py --out /tmp/delm-proof-interrupt --case interrupt
python3 scripts/verify_native_lifecycle.py --out /tmp/delm-proof-preflight --case preflight
python3 scripts/verify_native_lifecycle.py --out /tmp/delm-proof-stop --case stop
python3 scripts/verify_native_lifecycle.py --out /tmp/delm-proof-owner --case owner-death
python3 scripts/verify_native_lifecycle.py --out /tmp/delm-proof-remove --case plugin-remove
```

`stop` keeps the fixture alive for 65 seconds after native steering before allowing the scripted parent to finish. `preflight` interrupts after native execution yields but before any fixture child starts. The remaining cases check explicit cancellation, exact parent-process death, and removal of the fixture plugin. `--codex` selects a host binary; `--fixture` selects the compiled example. Each run retains `result.json`, native RPC messages, hook inputs, provider requests, and source/binary hashes.

The canonical launch supplies a fresh invocation UUID and a literal absolute project path. The PreToolUse hook records ownership without changing tool inputs or permission decisions. The runtime consumes the reservation once. One native turn can own only one consumed invocation because native cancellation events do not carry invocation IDs; after a failed consumed launch, send a fresh user message before invoking DeLM again. An unconsumed reservation can be replaced safely because the old command then fails its identity check.

These tests qualify the native event and ownership mechanism. They do not certify desktop/IDE behavior, production worker shutdown, the full supported-version matrix, or cancellation after hooks are disabled. On stock 0.159.3, revoking the active Interrupt/Stop hooks through native `config/batchWrite` suppresses those hooks; a subsequent native interrupt leaves the yielded fixture alive. A fresh `hooks/list` also does not prove the contents of another session's cached hook engine. The approved contract uses native events without a chat heartbeat: **stop active DeLM work before disabling or untrusting the plugin**. No configuration-watching fallback is implied.

DeLM does not register SessionEnd because its native input lacks a turn/invocation identity. Explicit Interrupt is durable; a prior-turn Stop is discarded when an active run's next user turn begins, so a delayed question reply cannot cancel the answered turn. Exact process identity and package-resource checks remain independent of model polling. The ordinary execution deadline remains separate from these ownership signals.
