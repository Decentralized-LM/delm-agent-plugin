# Codex installation and interface reference

Verified against the installed stock Codex CLI 0.159.3, this repository's current installation code, and Context7's `/openai/codex` documentation on October 3, 2026. This file records source-grounded details for the film. It does not change the plugin or install anything.

## Installation in the film

The film may use the explicitly approved future installer placeholder:

```sh
npx delm-agent-plugin
```

This package does not exist in this repository yet. A future npm package could download the correct released runtime and call Codex's own marketplace installer. It must not replace Codex, change the user's account, or bypass hook trust. Do not use `npx install "delm-agent-plugin"`: that executes a different package named `install`.

Codex already has a native installation command. Once the signed marketplace release exists, this repository's planned release path is:

```sh
codex plugin marketplace add jerry2247/delm-agent-plugin --ref marketplace && codex plugin add delm@delm
```

Current working installation is `./scripts/install.sh` from the source checkout. The repository is currently private; its public marketplace branch and prebuilt release are not yet published. Future npm installation should be a convenience wrapper around this native path, not another plugin system.

Native setup requires restarting Codex, reviewing and trusting the DeLM hooks, and restarting again. Show a short trust moment and an editorial cut into the ready Codex session. Do not imply the npm command automatically grants trust or that the plugin has an OpenAI directory listing.

Sources: `README.md`, `docs/releases.md`, `scripts/package_release.py:55-80`, `scripts/install_support.py:139-155`; locally verified `codex plugin marketplace add --help` and `codex plugin add --help`.

## Invocation

Use `$delm:run` in the user prompt. A bare `/delm` is not registered by stock Codex plugins. Native plugin skills use `$plugin:skill` namespacing. The skill picker can insert the mention, but no custom DeLM launch button exists.

Example editorial prompt:

```text
› $delm:run Build Neon Rush, a polished browser game with
  responsive controls, a hand-designed level, and synthwave audio.
```

This shortened line is editorial demonstration copy, not a transcript or measured run. For footage represented as an actual result, use that run's actual request and retained artifact.

Sources: `docs/support.md:17`, `skills/run/SKILL.md`; official namespace implementation and tests at https://github.com/openai/codex/blob/main/codex-rs/skills/src/mentions_tests.rs and https://github.com/openai/codex/blob/main/codex-rs/ext/skills/src/loader/namespace.rs.

## Welcome header

Native header shape from the local upstream snapshot:

```text
╭─────────────────────────────────────────────╮
│ >_ OpenAI Codex (v0.159.3)                   │
│                                             │
│ model:     <model> <effort> /model to change │
│ directory: <project path>                    │
╰─────────────────────────────────────────────╯

› Ask Codex to do anything

  ? for shortcuts                 100% context left
```

The version comes from the current installed CLI. Model and directory are data, not fixed native strings. Use the actual captured model/path when presenting real footage. If animating only a tight crop of the composer, omit the header rather than inventing model settings.

The terminal window title bar and macOS traffic lights belong to the terminal application, not Codex. A tilted terminal window is acceptable editorial framing. Keep prompt text flat and readable when it matters.

Local source root: `product-research/probes/codex-host/source/codex-rs/tui/src/`.
Snapshots: `snapshots/codex_tui__app__tests__clear_ui_after_long_transcript_fresh_header_only.snap`; `bottom_pane/snapshots/codex_tui__bottom_pane__chat_composer__tests__empty.snap`.

## Native hook trust screen

The shortest authentic screen is Codex's startup review prompt. Exact native wording, with the fixture's count adjusted to DeLM's four registered hooks:

```text
  Hooks need review
  4 hooks are new or changed.
  Hooks can run outside the sandbox after you trust them.

› 1. Review hooks
  2. Trust all and continue
  3. Continue without trusting (hooks won't run)

  Press enter to confirm or esc to go back
```

Animate keyboard selection from row 1 to row 2, then Enter. These are terminal rows, not clickable rounded buttons. Selection pointer `›` moves; the native interface has no green installation-success badge or floating permission checkmark.

Source: `startup_hooks_review.rs:229-251` and `snapshots/codex_tui__startup_hooks_review__tests__startup_hooks_review_prompt.snap` under the local source root. DeLM's actual definitions are in `hooks/hooks.json`.

If explicitly showing `/hooks`, use its native table instead. The real screen title is `Hooks`; its subtitle is `Lifecycle hooks from config and enabled plugins.` Columns are `Event`, `Installed`, `Active`, `Review`, `Description`. Its footer when review is required is:

```text
Press t to trust all; enter to review hooks; esc to close
```

The table includes all event types, including zero-count rows. DeLM supplies one each for PreToolUse, UserPromptSubmit, Stop, and Interrupt. Do not simplify this into a made-up DeLM-specific permission dialog. A tight crop of the actual startup review is cleaner for the film.

Source: `bottom_pane/snapshots/codex_tui__bottom_pane__hooks_browser_view__tests__hooks_browser_events_with_review_column.snap`.

## Actual DeLM progress text

These strings come directly from runtime events:

```text
Preparing two private workspaces. Your original project will stay unchanged.
Two DeLM workers are running and sharing progress.
```

Source: `src/run/mod.rs:437` and `:648`.

Codex does not gain a permanent custom worker dashboard from the plugin. Any two-agent collaboration animation should visibly leave the Codex window and become an explanatory scene. Keep the two workers as peers, with shared context and a task queue. The original project stays unchanged; a completed private project is returned. Do not show automatic in-place edits, a central boss agent assigning every task, or a fabricated performance comparison.

## Recommended opening

1. A copy action sends the future `npx delm-agent-plugin` command into a tilted terminal.
2. The camera settles as Enter executes the command.
3. A brief native hook review selection establishes that setup is explicit.
4. Cut into a ready Codex session and type `$delm:run` plus the task.
5. Show authentic progress text, then move out of Codex into the animated collaboration explanation.

Document the future installer placeholder in the demo README. Before a public launch, replace it with a released, tested command and record the actual installation sequence.
