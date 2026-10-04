# Claude Code board implementation plan

Status: implemented, with model-free native qualification on Claude Code 2.1.289. This document records the user experience, implementation boundaries, and validation.

## The experience

**Submit `/delm:run <task>` in Claude Code. A live DeLM board opens inside that same terminal. Keep using the normal prompt.**

The board answers three questions: what each agent is doing, who owns each task, and what useful information the agents have shared. It updates through application code. Displaying progress adds no model calls or messages to the conversation.

### Exact user actions

This starts with DeLM already installed and enabled, and Claude Code open in the intended project.

| What the user wants | Exact action | What appears |
| --- | --- | --- |
| Start work | Type `/delm:run <task>` and press **Enter**. | The board opens automatically, initially showing **Preparing**. It moves to **Starting agents**, then shows confirmed activity. No additional click or command is needed to watch it. |
| Continue giving instructions | Type the follow-up in the normal Claude prompt and press **Enter**. | The existing follow-up flow runs. The board reports forwarding and worker receipt only when those events are observed. |
| Inspect a task or contribution | Click that row. With the keyboard, press **Ctrl+X**, then **Tab** to focus the pane; press **Tab** again to select a row and **Enter** to open it. | Details replace the contents of the same board. **Back** returns to the previous view and brings the originating row into view. |
| Return to typing | Press **Esc** while the pane has focus, or click the normal prompt. | Focus returns to the prompt. The board stays visible. |
| Hide the board | Click **Hide board** or the pane's native close control. The native close shortcut is **Ctrl+X**, then **X**. | The large board closes. A compact DeLM status remains above the prompt. Work continues. |
| Reopen it | Type `/delm-status` and press **Enter**, or click **Show board** in the compact status. | The same run's board opens with its latest available state. This does not start a run or ask the model for a summary. |
| Answer a question or grant permission | Use Claude's normal question or permission interface when it appears. | Claude remains responsible for that interaction. The board yields space where necessary. |
| Stop work | Type `/delm-stop` and press **Enter**. | The board shows **Stopping** until shutdown and preservation have been confirmed. The existing stop behavior remains authoritative. |
| See the result | No extra action. | The board reports delivery and cleanup, and Claude provides the existing final handoff. The final board stays available until hidden or replaced by the next explicit run. |

The visible command set stays the same. `/delm-status` becomes a readable view instead of a JSON dump. Its description becomes “Show the DeLM board”; the stop description becomes “Stop DeLM and save unfinished changes.” No board setup command is added.

With no run in the current conversation, `/delm-status` returns **No DeLM run in this conversation**. It opens neither an empty pane nor an observer.

### Where the board appears

Use Claude's native `Pane`, opened during the original command invocation:

- In fullscreen terminals at least 110 columns wide, Claude places it beside the conversation, on the right.
- In narrower terminals and the classic terminal renderer, Claude places it above the prompt in the same terminal. It scrolls within the space Claude provides.
- Request a preferred width of 54 columns and inline height of 24 rows. Render from the actual `bodyColumns` and `scroll.bodyRows` supplied by Claude, rather than assuming those preferences were granted.
- Opening and refreshing preserve the prompt's keyboard focus and any text already entered. Do not request `focus`, `closeOnEscape`, or `holdToasts` for the automatic board.

The command-triggered open matters: Claude's threshold for unsolicited background panes differs. Invoke `ui.open` directly in the accepted `/delm:run` handler, start preparation without awaiting rendering, and catch presentation failures separately. Waiting until a background event to open the board could leave it undisplayed.

If another native pane is already selected, preserve its focus and use Claude's native pane tabs. Offer **Show board** in the compact status if DeLM's pane is open but not currently shown. Never force repeated activation during updates.

## One default layout

The overview contains these sections in order. Details stay inside the same pane.

| Section | Content | Presentation rule |
| --- | --- | --- |
| Run status | DeLM and the confirmed run phase; a short attention message only when needed. | One or two lines. No estimated completion time or invented progress percentage. |
| Agents | One stable row per agent: name, owned task, and observed activity or declared dependency. | Consistent **Agent 1**, **Agent 2** labels. The current runtime supplies two; the renderer consumes a list rather than assuming a fixed count. |
| Task queue | Stable task number, title, owner, and state. | Current and available work precede completed history. Keep order stable within each group; do not move a selected row while the user reads it. |
| Shared context | Recent findings and code contributions explicitly shared by an agent. | Show author and concise title. Show peer import only after a confirmed import. Ordinary tool calls do not become shared context. |
| Controls | **Details** and **Hide board**. | Native text buttons with consistent keyboard order. Additional actions appear only in the relevant detail. |

**Details** provides the complete task list, older shared entries, recorded checks, and delivery or recovery information. A task or contribution row opens its detail directly. Show “Showing 4 of 12” when abbreviating a collection, with **View all** access; do not silently drop older items.

On small terminals, prioritize run state and agents, then let the queue and shared context scroll. Do not squeeze everything into tiny columns. Empty sections use one honest line, such as “No tasks shared yet,” rather than large empty boxes.

### Visual treatment

Use the terminal's font, Claude's native frame and controls, normal foreground text, and one restrained blue accent for DeLM and selection. Use text labels as well as color for state. Check contrast in light and dark themes. Separate sections with spacing or a thin native rule, not nested cards.

Task titles may wrap; owners and states remain legible. Size by terminal cells, including wide characters, rather than JavaScript string length. Stable row keys preserve focus. Changed fields update in place without a typewriter effect, flashing borders, or a continuously running animation timer.

The hidden-board status occupies at most two lines above the prompt: a short run summary and **Show board**. Suppress it when the main board is actually shown. Respect `AbovePrompt.hasSurvey`, `maxRows`, and the native collapse control. Native questions take precedence.

### Honest examples

**Agent 1 · #3 Import endpoint · Working** requires both a confirmed task claim and an observed active native turn. If only the claim is known, show **Claimed**, not an invented activity. An agent-authored summary remains distinguishable from native lifecycle state.

**CSV validation contract — shared by Agent 1** may later gain **Imported by Agent 2**. A finding has no import badge unless it is associated with an importable publication and a confirmed import. Sharing files is not proof that the combined project works.

## Lifecycle and edge cases

### Starting and follow-ups

Validate arguments and reject duplicate starts using the existing checks. Once a new start is accepted, create a temporary view identity for the conversation and show **Preparing**. Replace it with the real run identity when preparation returns. Show **Starting agents** until native bindings and turns establish activity.

A preparation error remains visible with its concrete error and existing recovery instruction. Do not display fabricated tasks while waiting. A refused second start refers to the current run instead of replacing its board.

Tasks reflect committed state: available, claimed, and done. A separate dependency annotation explains why a task's owning agent is waiting; waiting is an agent state, not a new task state. **Done** means the agent declared the task finished; recorded verification evidence appears separately in its detail. A quiet agent is not automatically blocked.

Keep the accepted request revision while a follow-up is being admitted. After acceptance, display forwarding and each worker's confirmed receipt independently. Rejected attachments or references do not advance the displayed revision. A newer revision alone does not prove every agent received it.

One worker can wait while another continues. **Waiting for you** requires a supported native event identifying a pending user interaction. A long-running tool is insufficient evidence. If the host provides no reliable question or permission event, rely on its native interface and omit the inferred badge.

### Hiding and reading details

Store visibility per conversation and run. A manual hide survives ordinary progress, reconnection, and module reload. A new explicit run opens its board again. Closing a pane never calls cancellation or recovery.

Keep detail selection and reading position while events arrive. A detail is a captured record: when newer board state arrives, show **New updates · Refresh**. Refresh reads the selected item directly, or refreshes a collection at its current page with the latest record set, without changing the run. Next and Previous retain their collection boundary while reading. A page from an earlier request also identifies its request revision. Do not scroll to the newest entry while the user reads earlier work. Opening a detail reveals its start; **Back** reveals the originating row. The native scroll API exposes semantic anchors, not arbitrary row offsets, so returning restores the selected content rather than promising the exact previous pixel or row position. Native **Esc** returns keyboard focus; do not redefine it as detail navigation.

If the pane cannot be placed, use compact status and a readable `/delm-status` fallback. If observation or rendering fails, the run continues and the status command still returns a short text summary from the passive view data. Report the view failure once, without turning it into a run failure or repeatedly emitting notices.

### Completion and cancellation

| Confirmed condition | User-facing meaning |
| --- | --- |
| A worker proposes a complete candidate | **Finishing**. Native work may still require shutdown; project delivery has not been established. |
| Changes reached the original project | **Changes applied to your project**. Required local verification is a separate fact. |
| Delivery requires an original-project check | **Changes applied · local verification required**. Direct the user to Claude's final handoff for the outcome; this board does not currently record that later check. |
| Runtime completion and cleanup are confirmed | Show the confirmed completion outcome and **Temporary workspaces removed** in result details. Retain any unresolved verification limitation. |
| A stop request is accepted | **Stopping**. Continue observing the existing stop handler; do not show saved work or completed cleanup before confirmation. |
| Shutdown and recovery saving succeed | **Stopped · unfinished changes saved**. Details explain that changes were saved for recovery rather than automatically applied. |
| Shutdown is unconfirmed or saving fails | **Needs attention** with the confirmed error and retained-work location. Never claim cleanup succeeded. |
| Delivery encounters a project conflict | **Delivery needs attention** with conflict information and recovery location. Do not report the project as updated. |

The stopped-run detail explains recovery concretely: saved changes remain in a local recovery bundle; temporary worker folders are removed after shutdown and saving succeed. If safe cleanup cannot be established, folders remain. This is not a paused session that simply resumes. Do not add a **Resume** button.

Preserve existing parent launch, update/resume, and final handoff behavior. The board adds zero presentation-only model turns. The current adapter does not associate the parent's later checks with a board outcome. Retain the verification-required limitation in the final board and refer to the final handoff, rather than leaving an indefinite **Checking** indicator. Do not clear it from a parent answer or generic turn completion, generate a check, or ask the parent to report one merely to fill a status field.

### Reloads and conversation changes

Bind every view, callback, detail request, and preference to conversation ID, run ID, and a local view generation. Clear old visible data before attaching another conversation. Late responses cannot repaint the new conversation. Existing project ownership checks remain authoritative; the board offers no controls for another conversation's run.

On return, show the saved final result or current recovery state. The existing session lifecycle handles interrupted-run recovery; merely opening or refreshing the board must not start recovery, stop workers, or resume a run. Read finished runs from saved results after the live bridge exits.

Implement a side-effect-free view lookup for `/delm-status` and its text fallback. It reads the in-memory binding or saved record directly; it must not reuse `currentRun($)`, which can enter recovery, or the existing control `status` operation. Session lifecycle restoration remains separate.

When viewing a native subagent transcript belonging to the run, retain the same run board and identify that agent. For an unrelated agent, suppress the run's compact status rather than suggesting the agent belongs to DeLM.

After a view connection fails, retain the last snapshot with **Updates disconnected** and its observation time. This does not establish that the run stopped. Reconnect with bounded retries, replace state atomically, and preserve manual hiding. Exhausted retries leave `/delm-status` available for another attempt. At confirmed completion, end refresh work and retain the final snapshot.

## Engineering design

### Constraints addressed

- `src/claude/controller.rs`: `status` currently increments the controller sequence and persists state. Screen refresh must not use it.
- `src/claude/mod.rs`: the stream emits readiness and control actions. A task claim or shared finding need not emit an action; this stream alone cannot power the board.
- `src/board/mod.rs`: `Board::open` initializes storage and acquires an exclusive lock. Board operations also use that lock. The viewer needs separate read-only access.
- The agent-facing board view caps collections at 24 and truncates text. Human totals and details need their own queries; worker responses stay unchanged.
- `src/services.rs`: listing services can refresh hashes and change recorded state. Saved lifecycle data contains process identities, not a complete current service registry. Service detail is outside this board version; refresh never invokes service listing.
- `hosts/claude/hooks/delm.js`: stream errors enter `reportFailure`, which can reject delivery gates. View parsing and rendering require a separate error boundary.

### A passive observer

Add a read-only observer through an internal `claude view` command. The plugin owns this helper process; the user does not launch it. Keep one child stream while the board is live, avoiding repeated process startup. Its display output is independent of the existing control/action stream.

Read existing durable state and emit allowlisted snapshots and bounded activity pages. Do not call `Controller::handle`, open another `Board` owner, acquire the coordination lock, refresh service health, or invoke tools. This needs neither a new server port nor a writable event database.

Use a dedicated SQLite connection opened read-only, query-only queries, and short transactions. Read tasks, counts, publications, findings, and confirmed import/check records. Read saved lifecycle metadata in bounded operations. Where journal evidence is needed, consume complete records incrementally using byte offsets; never forward raw records or continuously rescan full logs. Commit every read transaction before writing to the observer's output stream.

Native observations also live in the JavaScript adapter: for example, worker update-delivery receipts are in `run.agents` and plugin storage. Merge allowlisted notifications from already-successful native operations with the runtime snapshot in `board-view.js`. Give that source its own monotonically increasing view revision. Reopening reads saved receipts without invoking lifecycle restoration. Do not treat the controller's accepted request revision as a native delivery receipt.

Validate the owned run directory, session binding, file types, and symlink boundaries with the existing private-storage conventions. Accept only the selected run, not an arbitrary project path. Keep any required authentication material on stdin and out of arguments, snapshots, errors, and UI preferences.

Saved lifecycle metadata and SQLite do not form one atomic transaction. Include source cursors, reject backwards updates, and withhold combined claims until their prerequisites are observed. A temporarily busy or incomplete source retains its last valid state with a freshness indication. Never seek perfect consistency inside a worker or control path.

Keep WAL read transactions short: no cursor remains open across rendering or user interaction, and a stalled viewer cannot hold a long transaction or prevent cleanup. If contention occurs, back off the observer rather than delay the writer. Measure this in concurrency fixtures.

### Display contract

| Field group | Required information |
| --- | --- |
| Identity | Schema version, conversation/session ID, run ID, accepted request revision, view generation. |
| Ordering | Controller sequence, board sequence, native observation revision, and journal cursor where used. These are separate clocks. |
| Freshness | Observation time, live/restored/disconnected state, unavailable sources. |
| Agents | Stable identity and display name, native state, claimed task IDs, explicit reported status/dependency, confirmed update receipt. |
| Tasks | ID, title, owner, state, version, dependencies, collection total, page cursor. |
| Shared context | Publication or finding ID, author, concise shared text, revision, confirmed peer import references. |
| Evidence | Recorded check outcome, scope/revision, and validity for reuse. Detail-only by default. |
| Outcome | Delivery, pending verification, conflict, cleanup, and recovery facts independently. |

Unknown is a real state, not a value to fill with narration. Relative event times require source timestamps; observing an old record now does not make it a new event. Where a timestamp is unavailable, show ordering without an age.

Use current database state for ownership. An idempotent tool replay can return a historical response; it must not roll tasks or revisions backwards. Pagination remains stable under additions, and abbreviated collections disclose their full totals.

Exclude control tokens, sockets, one-use tickets, raw commands and outputs, private worker answers, unpublished files, and full request bodies. Shared text is task data, never instructions to the renderer. Strip terminal controls, bound lengths, and handle Unicode safely. Recovery paths belong in outcome details.

### File responsibilities

| File | Planned responsibility |
| --- | --- |
| `src/claude/view.rs` (new) | Display schema, lifecycle projection, observer, bounded output, source reconciliation. |
| `src/board/reader.rs` (new) | Short read-only queries, complete counts, stable pages; no worker filesystem access or initialization. |
| `src/claude/mod.rs` | Internal view command dispatch; preserve the control transport. |
| `hosts/claude/hooks/board-view.js` (new) | Observer attachment, cached state, visibility, detail requests, bounded retries, disposal. |
| `hosts/claude/hooks/board-render.js` (new) | Pure native rendering, navigation, and accessible controls. |
| `hosts/claude/hooks/delm.js` | Small launch/status/session integration points and notifications of already-observed native state. |
| `hosts/claude/tests/board-view.test.mjs`, `board-render.test.mjs`, Rust view tests, and `scripts/test_claude_board.py` | Contract, ordering, isolation, native interaction, and real observer coverage. |
| `scripts/build.py` and packaging tests | Explicitly include new modules in the staged payload and adapter digest. |

Render through `ui.render`, `$.ui.resolve`, and native `Box`, `Text`, and `Button` components. Rendering reads cached state only: no filesystem reads, process launches, control requests, or model calls. Reconcile visibility with `$.ui.panes()` outside render; a local boolean alone is insufficient after reload or native closure.

Use stable pane ID `delm`; opening again reuses it. Observe `ui.close`, distinguishing user dismissal from unload or rendering failure. Never veto a user close. Save visibility under a separate UI store key, not through the runtime controller.

Details use native buttons with stable keys and cancel read-only queries on conversation change. Handlers update selection and invalidate the view without submitting prompts. Existing stop behavior stays independent of pane visibility.

Keep command registration independent of renderer initialization. Claude Code 2.1.289 accepts one hooks module and static imports only: native validation refuses multiple `modules` entries and dynamic `import()`. Import pure view modules statically, with no fallible startup work at module scope, and guard view initialization, observation, and rendering separately from run control. Runtime presentation failures must leave command handling and the passive text fallback available. These guards cannot isolate malformed module syntax or a native module-load refusal; strict package validation must catch those failures before distribution.

Use the installed observer binary to read supported saved-state versions, including records created before the board existed. Do not assume a retained executable in an older run understands `claude view`. Unknown saved-state versions get a readable, passive fallback without rewriting the record or entering run recovery. Detail pages use bounded read-only requests to the same observer implementation; they do not enter the control request lane.

### Performance and failure boundaries

- Initially cap visible observation and rendering at four refreshes per second, coalescing unchanged state. Allow one refresh in flight and one pending replacement.
- Reduce hidden observation to compact summaries, initially at most once per second. End it after a final result. Start no observer for a session without a DeLM run.
- Bound records, pages, queued bytes, and retries. A slow reader may miss intermediate visual updates but must converge to current state; recent activity remains available through pages.
- Catch UI errors separately. Never route them into `reportFailure` or reject a delivery gate because the view broke.
- Never await rendering or observation inside worker tool, completion, wakeup, permission, or follow-up handling. Copy small native observations after existing operations succeed.
- Measure observer CPU, memory, refresh latency, and worker/control latency with the view shown, hidden, disconnected, and deliberately slow. Compare the same deterministic workload and tune intervals if contention appears.

The implemented observer uses 250 ms sampling while visible and 1,000 ms while hidden, and a single-thread runtime. Snapshots stay below 240 KiB against a 256 KiB transport limit. Large pages shorten by whole rows while preserving complete row details, totals, and contiguous navigation. These limits bound observation; they are not a claim about task completion speed.

## Implementation sequence

### 1. Produce the actual native preview

Adapt the existing native research fixture to this one layout. Use deterministic sample events for preparation, claims, sharing, import, waiting, completion, and cancellation. Load it through the native plugin system in disposable configuration, without a model turn.

Deliver one short terminal recording showing launch, detail, hide, and reopen, plus native captures at wide and narrow sizes. Label sample data. Use the renderer module intended to ship so visual review applies to real terminal rendering, not a separate image. Review this before runtime wiring.

### 2. Implement the read-only data path

Build schema, reader, and observer. Test ownership, explicit sharing, confirmed imports, counts beyond 24, revisions, and final outcomes. Establish that observation changes neither controller sequence, tasks, coordination events, tickets, service state, nor worker-facing responses. Test concurrent writes and cleanup.

### 3. Connect the native view

Attach observation after preparation without delaying launch. Keep **Preparing** local until the run ID exists. Wire command-time opening, status reopening, detail navigation, compact fallback, and visibility persistence. Preserve admission and functional lifecycle handlers.

### 4. Finish interaction and lifecycle behavior

Exercise follow-ups, questions, permissions, competing native panes, hiding, resizing, connection loss, reload, stop, conflicts, and completion. Include a saved older run opened by the newly installed plugin. Conversation changes invalidate all outstanding callbacks. Include attention badges only after qualifying the relevant native events.

### 5. Package, document, and review

Update package allowlists and hashes so installed packages contain both new modules. Verify loading with the minimum supported Claude version and current version. Use strict native plugin validation; it establishes package validity, not rendering or lifecycle correctness.

Update `docs/claude-integration.md`, the Claude instructions in `README.md`, and relevant support/development documentation with the exact start, hide, reopen, and stop flow. Keep diagnostic internals out of quick-start instructions. Document the native UI fixture for future regression checks.

Run the existing repository verification command and focused native UI/packaging checks. Present integrated-package captures and an acceptance report before committing the feature. A long application-building run is not required to test this presentation layer.

## Qualification results

The model-free fixtures exercise the shipped renderer, view adapter, and compiled observer. The native fixture supplies a saved sample run through an immediate command; it never asks a model to do work.

| Area | Verified behavior |
| --- | --- |
| Native layout | Wide fullscreen, narrow fullscreen, classic renderer, light and dark themes. Compact layouts keep agents, task and shared-context access, and controls visible within the available rows. |
| Native interaction | Automatic command-time opening; keyboard task detail and Back; hide and reopen; unsent prompt text remains intact through updates and navigation. Back reveals the originating task. |
| Observer | Real executable reads live claims, explicit findings, publications, and confirmed imports; finished helpers exit; unsupported versions and mismatched sessions fail passively. FIFO sources cannot block opening, journal tail boundaries retain complete records, and missing final evidence is explicit. |
| Isolation | Viewing leaves source records unchanged. A stalled observer does not hold a SQLite transaction across output or block task writes/checkpoints. A finished database gains no WAL or shared-memory companions. |
| Ordering and identity | Independent source clocks reject historical snapshots; delayed pane opens, observers, pages, and session identity queries cannot repaint another conversation. |
| Details | Collections beyond 24 records, Unicode byte budgets, dynamically sized forward/back pages, stale-page rejection, captured-detail provenance, exact-item refresh, refreshed collection boundaries, and semantic navigation. |
| Lifecycle | Preparation, waiting, stopping, delivery, verification limitations, recovery, cleanup, connection loss, bounded retries, hidden reload, and passive finished-run restoration. |
| Native observations | Observed turns and accepted request revisions remain distinct from worker receipt. Runtime waiting and final states take precedence over stale activity. |
| Presentation safety | Plain terminal controls; Unicode cell widths; bounded sanitized text; survey precedence; no invented permission state; unrelated-agent views suppressed. |
| Packaging | Both view modules are included in adapter hashes and staged packages. Prebuilt runtimes must supply both the native MCP transport and the board observer interface. The complete package passes native module validation on Claude Code 2.1.289. |

Reproduce observer, module, and packaging coverage with `./scripts/verify.sh`. Reproduce native captures and interaction tests with the [native board fixture](../tests/fixtures/claude-board/README.md). Capture metadata includes source hashes so a screenshot can be matched to its renderer, adapter, and binary.

Local review artifacts are in `.validation/claude-board/`: `release-wide/` and `release-small/` contain the actual adapter/observer captures using the staged optimized executable, including keyboard navigation and an 80×24 light terminal. `stress-small-final/` covers long and Unicode titles in the six-row layout; the other layout fixtures cover classic rendering, dark theme, and final outcomes. These generated files and disposable host settings are not distribution assets.

The final local pass completed the repository verification suite, 97 Claude module tests, 9 real observer tests against the optimized executable, 3 native interaction tests, and strict package validation. The additional review covered exact-item refresh, new rows arriving during pagination, startup hiding, delayed preference writes, and file-type or missing-evidence failures.

`observer-metrics.json` records two short samples per mode against a debug binary, with 32 large records and 64 paced writes. Observed peak memory was 9.8–13.6 MiB; maximum CPU was 5.9% of one core including startup. Writer p95 remained below 0.95 ms in every mode, and all truncating WAL checkpoints completed without contention. This small synthetic sample establishes resource scale and the tested isolation boundary; it is not a task-speed benchmark.

No account-backed agent task was run for this presentation change. Competing-pane visibility, native survey precedence, reload races, and lifecycle failures have contract-level tests; the terminal captures establish the specific interactions and layouts listed above, not every terminal emulator or future host release. The minimum supported and locally installed host were both 2.1.289.

## Evidence and scope boundary

The implementation builds on the native preview and source audit. Claude Code 2.1.289's generated declarations define `PaneOpenArgs`, `Pane`, `AbovePrompt`, `ui.open`, `ui.close`, `ui.panes`, and `ui.invalidate`. Integrated model-free native captures now establish docked and inline rendering from the real passive observer and adapter, using sample run data.

The [official plugin manifest reference](https://code.claude.com/docs/en/plugins-reference) documents packaging and strict validation. The [native UI documentation address](https://code.claude.com/docs/en/plugins/mods/interface) was not retrievable during review, and the documentation index queried through Context7 did not cover the pane APIs. Placement and focus details here rely on the installed generated API and native fixture; recheck them against the exact versions used for qualification.

Agent prompts, task selection, shared-context policy, verification policy, and model-visible information remain explicit review boundaries. This work observes existing collaboration. Changes to those behaviors require a separate proposal under the [product and runtime plan](product-runtime-plan.md).
