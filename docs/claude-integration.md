# Native Claude Code integration

Claude Code and Codex share DeLM's worker policy, task board, contribution format, check receipts, service ownership, workspace preparation, and guarded delivery. Each host supplies native execution and lifecycle events. Claude's resources live in `hosts/claude/`; its controller and transport live in `src/claude/`.

## Native execution

The official `/delm:run` skill is explicit-only. A plugin module intercepts it, checks the MCP connection, and prepares the project before the parent makes a short launch turn. That turn issues exactly two native `fork` Agent calls together. Spawn middleware assigns the two prepared directories and binds each returned agent identity. Forks inherit the conversation, system prompt, model, and available native tools. The worker policy appears once in the inherited context, with a short pointer in each launch call.

The parent launch turn is deliberate. On Claude Code 2.1.289 in Auto mode, direct plugin-origin `$.agent.spawn` has no server classifier verdict and is refused. Native model-issued Agent calls receive the ordinary permission review. DeLM uses this supported path without changing permission mode or adding allow rules. Finished peers resume through native SendMessage under their existing identities. Active peers receive context updates through native session append.

## Coordination and permissions

The official `.mcp.json` sidecar exposes DeLM's common coordination tools. Loading the plugin starts this lightweight transport, not a team. A native tool-call hook reserves a one-use ticket for the observed agent, turn, revision, tool, and exact arguments. It then calls `next` so Claude performs its own tool permissions, approvals, and MCP dispatch. The sidecar can consume only that matching ticket. It cannot trust worker identities supplied by the model.

Board transfers remain confined to the prepared private copies. These are additional MCP capabilities, with their own native permissions; native Read or Edit deny rules are not an operating-system sandbox around plugin code. Ordinary worker tools keep their host permission handling. This is a trusted local development plugin.

Native Bash observations supply check evidence. Claude's interface exposes a tool result reference and completion state, not a numeric exit code. DeLM records that distinction. Missing, interrupted, timed-out, and background outcomes cannot qualify a completed check. Relevant file changes invalidate shared evidence.

Shared checks validate their explicitly named input files independently of the source and artifacts selected for delivery. An installed dependency can be a valid check input without being copied to the original project. Inputs are rechecked after shutdown and before first delivery, including during recovery using saved native file permissions. A retry of an already completed delivery uses its saved journal and preserves later user edits.

## Lifecycle and delivery

Each peer can claim, split, publish, import, verify, and integrate work. Both contribute to one assembled result. There is no separate Claude collaboration algorithm or mandatory duplicate full-suite pass.

The module records peer identities, their descendants, and native background-task IDs. A completion candidate creates a durable finalization generation and request revision. Before stopping any native task, the controller closes update admission for that generation. An update accepted first invalidates the older finalizer; a later update is refused accurately. Settlement confirms terminal states and stopped services, then checks for live references to the private trees. DeLM's own runtime helpers and board observers start outside those trees, so they never hold such a reference themselves. The runtime's final result settles the run: bookkeeping for a stopped peer that arrives after the runtime has exited cannot mark a delivered or stopped run as needing recovery. A successful native stop acknowledgment may precede the host's updated agent status; bounded native polling handles that interval without assuming shutdown. Failed settlement has bounded retries, and later native shutdown evidence can trigger another attempt.

Known background shell tasks stop before their owning agents. Native task-notification fields can establish that an exact recorded task already completed, failed, or was killed; arbitrary output text and not-found errors cannot. Notification observation does not require opening the board, and the operating-system writer check still gates cleanup. The board's explicit retry uses a session-bound host action closure; native API context is not passed through a dynamic cross-module callback.

Only then does shared delivery revalidate and apply the accepted source and declared artifacts to the original project, preserving its Git index and unrelated edits. Conflicts save recoverable work. Additional custom output is preserved for review; explicitly accounted-for source and artifacts can still be delivered successfully. After safe delivery or recovery, both worker copies and the temporary baseline are removed. Dependencies remain local to each project, so the parent performs necessary setup or a focused relocated-result check when the delivery report requires it. A retained completion candidate keeps its delivery intent across interrupted settlement; explicit cancellation deliberately switches to saving partial changes.

Run ownership is durable and keyed by the native conversation ID. Commands and runtime callbacks recheck that identity; clearing, resuming, or branching cannot carry status, updates, or queued control messages into another conversation. Ending a conversation requests cancellation within the host's short shutdown budget. Returning to it restores its saved outcome or performs guarded recovery. Recovery confirms the old bridge and owned execution have stopped. An unfinished Claude run also blocks a new Codex run on that project. Uncertain shutdown or an unreconstructable interrupted capture preserves the copies instead of guessing what is safe to remove. `/delm-stop` remains the explicit cancellation command.

## Native progress board

`/delm:run <task>` opens the board automatically inside Claude's terminal. Fullscreen terminals at least 110 columns wide place it on the right; narrower terminals and the classic renderer place it above the prompt. Continue typing in the normal prompt. Click a task or shared contribution for details and use **Back** to return. **Hide board** leaves the run active; `/delm-status` or **Show board** reopens it. With no run in the conversation, `/delm-status` returns a short message without opening a pane.

The board uses the native `Pane` and compact `AbovePrompt` surfaces. `hooks/board-view.js` owns the conversation-bound view and observer connection; `hooks/board-render.js` presents its snapshots. An internal `claude view` process reads existing run records and the task database without invoking the controller or worker tools. It opens only a run directory that the current user owns and other users cannot access, inside DeLM storage folders that other users cannot change; storage folders created by earlier releases with ordinary read permissions remain viewable. Native adapter observations supply confirmed worker state and update receipts. Display updates add no presentation-only model turns, change no task policy, and do not expose private tool output as shared context.

After the native module loads, view errors remain separate from run failures. Opening, hiding, refreshing, and reading details do not invoke cancellation, recovery, or resumption. A disconnected view retains its last snapshot with a freshness notice and the observer's stated reason, and permits another attempt through `/delm-status`. Manual hiding persists for that run; a new explicit run opens the board again. Final state reports delivery, cleanup, and recovery independently. A local verification requirement remains visible because the board does not observe the parent's later checks; Claude's existing final handoff supplies that outcome.

The implementation and native qualification checklist are in the [board plan](terminal-ui-plan.md). Model-free rendering and lifecycle fixtures establish their exercised cases; interactive keyboard, focus, theme, and terminal behavior require native qualification against the installed host.

## Follow-up instructions

Text updates include the prompt and additional text context accepted by Claude's prompt middleware. A rejected native submission does not advance the DeLM request revision. Both peers receive each accepted update; their next model requests acknowledge it only after complete native delivery and a fresh turn. If native middleware refuses or changes a required context append, DeLM pauses rather than reporting that the full update arrived.

Owning worker resources is separate from owning conversation input. Only a healthy working run forwards normal prompts. Finishing, stopping, failed and restored recovery states pass ordinary input back to Claude unchanged, while preventing old peers from starting new work. An undelivered update is not replayed in the parent. Recovery and its storage or transport failures cannot install a persistent parent-turn prohibition.

Claude notifies the parent conversation each time a forked peer stops, including when DeLM stops it during settlement. DeLM keeps these notices for its own peers out of the parent and shows a one-line note instead: the board reports peer progress and the final handoff reports the outcome. Admitting each notice would start a parent turn that narrates stale peer status, even after delivery, and holds DeLM's resume controls behind that turn. Notices for other tasks, recorded background shells, and agents that a peer starts reach the parent unchanged.

Claude Code 2.1.289 exposes attachment metadata on `prompt.submit`, and its `session.append` API accepts text blocks only. DeLM therefore rejects follow-up images, audio, and documents before changing the task. The error explains that workers retain the previous task and offers the supported route: stop the run and include the attachment in a new `/delm:run`, whose native forks inherit the initial conversation. Unexpanded `@reference` submissions receive the same guidance; paste the relevant text when continuing an existing run. Email addresses, quoted literals, and code are treated as ordinary text. Already supplied textual selections and context are preserved.

These limitations apply to forwarding new inputs into existing peers, not to what native forks inherit at launch. DeLM does not change native permissions, replace skills, select a different model, or read referenced files outside the host's normal tool permissions to work around them.

## Implementation and qualification

The implementation followed these stages:

1. Qualify native forks, inherited context and tools, private working directories, active updates, completed-agent resumption, and exact owned-task stops against the installed host.
2. Extract host-neutral evidence and filesystem scopes while preserving Codex's existing wire records and behavior.
3. Implement the native Claude module, authenticated bridge, revision-bound board calls, and guarded lifecycle using the shared runtime.
4. Use one installer entry point with host detection, a choice when both hosts are available, and explicit host flags for scripts. Each adapter handles native marketplace registration, update, status, and removal.
5. Stage self-contained host packages from explicit allowlists, validate through the official CLI, and bind release qualification to the actual runtime and adapter bytes.
6. Exercise lifecycle changes with focused regression tests and native package validation. Qualify account-backed collaboration separately using the fixture documented below.

Native boundary checks use Claude Code 2.1.289 on Apple Silicon with the existing Auto permissions. They establish inherited conversation markers, project instructions, skills, model and observed tools, actual worker working directories, active updates, same-identity resumption, and exact native agent/background-task stops. The real collaboration fixture and its evidence format are documented in [development](development.md#claude-code). It requires useful publications from both workers, checked output delivered to the original project, preservation of original files and staging, and removal of both worker trees.

A small fixture establishes the exercised behavior, not universal task quality or a speedup guarantee. Intel native qualification and checks against the exact signed release candidate remain release requirements. [Release qualification](releases.md) distinguishes model-free CI checks, development fixtures, and publication evidence. Routine tests do not use an account or change the user's saved host settings. The conversation-lifecycle changes were checked with deterministic regressions, native API validation, and distribution checks; an account-backed task was not repeated for this revision. The host regression suite exercises conversation changes without a new `session.start`, delayed callbacks, recovery isolation, rejected inputs, accepted context rewrites, both-peer delivery, and inherited fork settings. Native `claude plugin validate --strict` checks the adapter against the installed host's supported events and APIs.

## Official references

- [Plugin module APIs](https://code.claude.com/docs/en/plugins/mods/reference)
- [Generating types for the installed host](https://code.claude.com/docs/en/plugins/mods/create)
- [Native conversation forks](https://code.claude.com/docs/en/sub-agents#fork-the-current-conversation)
- [Server-side Auto permission review](https://code.claude.com/docs/en/permission-modes#server-side-classifier-review)
- [Plugin manifest reference](https://code.claude.com/docs/en/plugins/manifest-reference)
- [Native marketplace reference](https://code.claude.com/docs/en/plugins/marketplace-reference)
- [Native plugin CLI](https://code.claude.com/docs/en/plugins/cli-reference)
