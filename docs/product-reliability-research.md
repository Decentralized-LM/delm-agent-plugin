# Product reliability investigation and implementation plan

Investigation: 2026-10-04–05

Audited revision: `20a2a35e6313801f8ffef551546e9fbb4c437ab9`

Scope: both host integrations, shared runtime, delivery, recovery, coordination, installation, and the Claude board.

Status: this document records the investigation and implementation plan. Implementation and local verification are recorded in [Product reliability implementation and verification](product-reliability-verification.md), on `fix/plugin-reliability`. Findings below describe the audited revision rather than asserting that fixed defects remain in the working tree.

## Decision

The current product needs a reliability pass before broader distribution. The reported failed handoff is credible, and the current source contains independently confirmed reliability defects. Investigation also found defects outside the screenshots, including a reproduced case where a requested generated file is lost during an otherwise successful delivery.

The highest priorities are:

1. Preserve and deliver the user's actual outputs, including intentional generated artifacts.
2. Make completion, cancellation, shutdown, and recovery converge to a usable, truthful outcome.
3. Prevent accepted follow-ups from being discarded during finalization.
4. Make the board accurately represent execution and make all coordination records discoverable.
5. Correct cross-host environment handling and qualify the complete lifecycle through production components.

These are correctness issues, not reasons to replace decentralized peers with a planning manager or to add another full test pass to every user task. Preserve fast independent startup, native capabilities and permissions, selective sharing, and reuse of applicable verification.

## Evidence and limits

The tester says the reported installation is current. This audit therefore investigates the current implementation without attributing the failure to an old installation. Local inspection found Claude Code `2.1.289` and Codex CLI `0.160.0`. The tester's exact binary/adapter hashes and original run events were not supplied.

Evidence labels used below:

| Label | Meaning |
| --- | --- |
| Reproduced | An isolated, model-free probe demonstrated the current code's behavior. Host fixtures establish behavior for the supplied event ordering, not that this was the tester's exact ordering. |
| Source-confirmed | The relevant behavior follows directly from the current implementation; the original failing run was not replayed. |
| Conditional risk | A supported or plausible condition exposes a mismatch that still needs a focused native qualification. |
| Unresolved | The screenshot and current code do not determine the actual initiating event. |

No account-backed agent task, installed-plugin change, active-session intervention, commit, or push was performed for this investigation. Product code and the promotion video were left unchanged. Small probes used disposable projects and retained local evidence under `.validation/product-audit-2026-10-04/`.

The existing Claude JavaScript suite passed **97/97** checks during this audit. Its 35 host tests are included in that total. Passing that suite does not contradict the new defects: several depend on event order or on combinations of components the existing tests do not exercise together.

## What the tester's screenshots actually establish

### Follow-up incident report received during implementation

The tester supplied a report for run `a15ea1e2-65fa-4677-ab43-6655cc8df305`. These are reported observations, not a locally replayed raw trace. The report locates the completed 15 MB, 4:15 video in worker 2's retained `video/out/delm_explainer.mp4` and says delivery never ran.

| UTC | Reported event |
| --- | --- |
| 04:23:38 | Explicit invocation and two forks launch. |
| 04:37:14 | Worker 2 backgrounds `sleep 300; echo waited` while waiting for its peer. |
| 04:37:55 | Worker 1 publishes scenes, then declares a dependency wait. |
| 04:39:47 | Worker 2 completes the render and records check receipt 36. |
| 04:40:23 | Worker 2 declares completion while its background sleep remains active. |
| 04:40:27 | A queued control asks the parent to resume worker 1. |
| 04:40:39 | Shutdown stops the agents before their recorded background shell; the shell's subsequent stop request lacks the required acknowledgment. |
| 04:53:38 | The execution deadline triggers another unsuccessful shutdown attempt. |
| 06:29–07:15 | Ordinary prompts are intercepted, including after the runtime becomes unreachable. |

This substantially narrows F02/F04/F17: owner-first shutdown can kill a child before its separate stop request, and the expected late SubagentStop evidence may never arrive. Bounded retries alone are insufficient. Reconcile already-terminal tasks using structured native evidence and stop known children before owners; retain the OS writer check before delivery. Neither arbitrary output text such as `[killed]` nor a generic not-found error alone proves termination.

The queued resume also needs revision/phase checks at actual execution, not just when the action is created. The worker contract should use a named dependency wait instead of peer-waiting shell loops. This does not justify blocking ordinary sleeps used by legitimate task code. The report's retried task-creation errors motivate explicit schema limits and publication-ID examples. The report does not establish F01 as this incident's cause; the requested video remains preserved in this run.

### Q2: agents start before the task queue contains tasks

The shared board starts without task rows. The workers create them; there is no precomputed task plan. The common worker instructions already require inspecting just enough to identify useful contributions and claiming before substantial implementation. Brief initial reconnaissance before a first claim is therefore legitimate.

An empty queue during sustained implementation would be a different problem: missing task creation, a visibility failure, or worker instructions not being followed. The screenshot alone does not distinguish these. It does not establish how long the queue was empty or what tools had run.

Investigate the interval using first native step, first useful tool action, first task creation, and first claim. Display observed activity separately from task ownership. Do not fabricate tasks or delay both workers behind a parent planning phase to make the board look populated.

Sources: [`src/board/mod.rs:91`](../src/board/mod.rs#L91), [`plugin/worker.md:5`](../plugin/worker.md#L5), [`board-render.js:211`](../hosts/claude/hooks/board-render.js#L211).

### Q3: task numbers appear to skip #2 and #3

Task IDs currently come from the global board event sequence. Claims, publications, findings, and other events consume numbers too. Consequently, `#1`, `#5`, and `#8` can be the only three tasks that exist. A large task number does not establish the number of subtasks.

The human board also deliberately abbreviates large collections. It has counts, a **View all** route, and paginated details. Missing numbers and abbreviated lists are separate issues. Check the actual total before concluding records were omitted.

The presentation should improve: show an explicit task count and a stable human task number, while retaining existing internal IDs. Do not renumber IDs in the database or infer missing tasks. The worker-facing board has a separate, real discovery defect described in F08.

Sources: [`src/board/mod.rs:739`](../src/board/mod.rs#L739), [`src/board/mod.rs:809`](../src/board/mod.rs#L809), [`reader.rs:218`](../src/board/reader.rs#L218), [`board-render.js:205`](../hosts/claude/hooks/board-render.js#L205), [`board-render.js:299`](../hosts/claude/hooks/board-render.js#L299).

### Q4: both agents stop, nothing reaches the project, and follow-ups fail

The screenshot shows three distinct facts:

- Native worker execution stopped.
- DeLM could not confirm shutdown of a recorded background task, so it preserved the private workspaces.
- A later update was rejected because the runtime was already stopping.

Preserving work when writers may still be active is correct. Leaving the user without a reliable path from that state to delivery or recovery is not.

The exact update error is raised only when Rust's `stop_requested` flag is set. A valid completion candidate instead enters `awaiting_shutdown` without setting that flag. Thus, **a failed candidate handoff alone does not explain the whole screenshot**: cancellation or the no-active-worker path must also have occurred. The visible duration of roughly 17 minutes does not establish expiration of the configured 30-minute allowance.

Likewise, “shutdown is not confirmed” does not prove the background process was still running. It can mean the required acknowledgment was absent, refused, late, or addressed incorrectly. The screenshots alone left the original trigger unresolved; the subsequent incident report above identifies owner-first shutdown as the reported trigger.

Sources: [`controller.rs:369`](../src/claude/controller.rs#L369), [`controller.rs:721`](../src/claude/controller.rs#L721), [`controller.rs:747`](../src/claude/controller.rs#L747), [`controller.rs:802`](../src/claude/controller.rs#L802), [`delm.js:273`](../hosts/claude/hooks/delm.js#L273), [`src/config.rs`](../src/config.rs).

## Findings

Priority definitions: **P0** can lose a requested result and blocks release; **P1** can prevent correct execution, delivery, or coordination; **P2** concerns misleading presentation, supportability, or a narrower compatibility risk. Priority is independent of whether a finding caused this particular report.

### F01 — Successful delivery can delete a requested generated artifact

**P0 · Both hosts · Reproduced**

Completion captures a manifest that includes generated outputs. Successful delivery then constructs a different, source-oriented manifest: tracked files, nonignored files, and admitted baseline files. Newly created, untracked, Git-ignored output is excluded. Cleanup removes both worker trees after delivering that filtered manifest.

An isolated probe created a project that ignores `renders/`, then created `renders/requested-video.mp4` and an ordinary source edit in a worker. Calling the real `deliver_result` returned:

```text
delivered: true
cleanup_complete: true
verification_required: false
changed_paths: [source.txt]
requested video in original project: absent
worker workspace: removed
```

The probe uses sentinel bytes, not a rendered movie; media validity is irrelevant to the file-delivery defect. The loss mechanism applies to intentionally generated PDFs, exports, images, and other ignored deliverables too. This does not establish that the tester's video was ignored or deleted—their screenshot indicates preserved workspaces.

**Required direction:** distinguish source changes, requested deliverables, dependency environments, and disposable caches. Add explicit deliverable selection to completion and delivery, validate contained paths and identities, preserve native authorization boundaries, and report exclusions. Verify that every declared deliverable is durably present in the original project or a named recovery bundle before cleanup. If an output is only preserved for recovery, report incomplete delivery rather than `delivered=true`. Do not solve this by copying all ignored files, credentials, and dependencies.

Sources: [`result.rs:24`](../src/workspace/result.rs#L24), [`delivery.rs:112`](../src/workspace/delivery.rs#L112), [`delivery.rs:414`](../src/workspace/delivery.rs#L414). Evidence: `delivery/ignored-output-probe.rs`, `delivery/ignored-output.log` under the audit evidence directory.

### F02 — Late shutdown evidence does not restart a failed handoff

**P1 · Claude · Reproduced in the host fixture**

`stopOwned` requires proof that recorded agents and background tasks stopped. If a background `TaskStop` returns an unconfirmed result, `settle` reports an error and ends. A later `SubagentStop` snapshot can establish that the task has finished, but its handler only updates stored observations. It does not retry settlement. A completed result can remain undelivered after the missing evidence arrives.

The isolated reproduction injects one unconfirmed TaskStop result, then supplies a terminal snapshot. No settlement request or retry is scheduled. This proves the recovery gap, not why the tester's actual TaskStop failed.

**Required direction:** make settlement an idempotent operation driven by durable intent and fresh native evidence. Reconcile late completion, bounded retry, and reload. Unknown shutdown must still preserve work; it must not be treated as successful shutdown.

Sources: [`delm.js:279`](../hosts/claude/hooks/delm.js#L279), [`delm.js:321`](../hosts/claude/hooks/delm.js#L321), [`delm.js:592`](../hosts/claude/hooks/delm.js#L592).

### F03 — A follow-up accepted during finalization can be discarded

**P1 · Claude · Reproduced host ordering; source-confirmed runtime behavior**

The runtime accepts an update while a candidate exists, advances the revision, invalidates the old candidate, and emits context/resume actions. The host can still have `stopping=true` and a settlement callback already scheduled. It discards those actions, then stops the agents. The accepted update has changed runtime state without reliably reaching either peer.

The fixture sequence is candidate → accepted update → skipped context → both peers stopped → settlement attempted. This is a consistency defect at the admission/finalization boundary.

**Required direction:** make update admission and finalization mutually ordered. An accepted update must invalidate any older settlement generation and reach the required peers. Once irreversible closure begins, preserve the message visibly for the next supported action instead of acknowledging it and dropping its delivery. Repeated callbacks and stale actions must be harmless.

Sources: [`controller.rs:802`](../src/claude/controller.rs#L802), [`controller.rs:829`](../src/claude/controller.rs#L829), [`delm.js:220`](../hosts/claude/hooks/delm.js#L220), [`delm.js:638`](../hosts/claude/hooks/delm.js#L638).

### F04 — Host and runtime disagree after shutdown failure

**P1 · Claude · Source-confirmed**

On settlement failure, JavaScript clears its `stopping` boolean. Rust can remain in `stopping` or `awaiting_shutdown`. The host still intercepts ordinary follow-ups as updates to an active run, while Rust may reject them. A single recoverable shutdown problem becomes a conversation that repeatedly refuses useful work.

Completion and cancellation also enter the same host action branch; the initiating stop reason is not retained there. Native “stopped by Claude” notifications do not explain whether DeLM selected a result, timed out, lost a worker, or was explicitly cancelled.

**Required direction:** use explicit run phases and transition reasons rather than independently toggled booleans. Expose result-selected, shutdown-pending, delivery-pending, recovery-required, and stopped distinctly. Record and display the initiating reason and a valid next action.

Sources: [`delm.js:224`](../hosts/claude/hooks/delm.js#L224), [`delm.js:326`](../hosts/claude/hooks/delm.js#L326), [`controller.rs:761`](../src/claude/controller.rs#L761).

### F05 — The advertised stop/recovery route discards delivery intent

**P1 · Claude · Source-confirmed**

`/delm-stop` sends `cancel`; cancellation clears a selected candidate. For a completed result awaiting only shutdown confirmation, this changes the outcome from retrying automatic delivery to preserving partial changes. It preserves files, but loses the user's route to receiving the already completed result through the normal handoff.

The error “restart with the saved update after recovery” is also imprecise: the runtime rejects that update before reading or storing it. It may remain in Claude's conversation, but DeLM has no durable pending-update record that guarantees replay.

**Required direction:** distinguish retrying a delivery from intentionally stopping and preserving unfinished work. Keep candidate identity and verification evidence through recoverable shutdown failures. Show the available action inside the existing status/detail experience before deciding whether another public command is necessary. State whether a follow-up was delivered, retained for later, or rejected.

Sources: [`delm.js:388`](../hosts/claude/hooks/delm.js#L388), [`controller.rs:369`](../src/claude/controller.rs#L369), [`controller.rs:802`](../src/claude/controller.rs#L802), [`controller.rs:1035`](../src/claude/controller.rs#L1035).

### F06 — A transient persistence error permanently breaks later writes

**P1 · Claude · Reproduced**

Each host snapshot is chained onto `run.writes.then(...)`. Once one store write rejects, later writes attach to that rejected promise and never execute. Error reporting uses the same persistence path.

In the fixture, a one-time store failure caused two successive persistence attempts to reject, while the underlying store was invoked only once.

**Required direction:** preserve ordered writes and surface persistence failure, but allow an explicit retry or subsequent safe snapshot to execute. Do not admit new ownership-changing operations unless the required durable state is established. Test both failure before recording ownership and failure after the native operation has occurred.

Sources: [`delm.js:24`](../hosts/claude/hooks/delm.js#L24), [`delm.js:88`](../hosts/claude/hooks/delm.js#L88).

### F07 — The board can downgrade a working agent to “Ready”

**P1 · Claude board · Reproduced**

Spawn persists an agent with `turn:null`. The native step hook later updates the agent's turn and status but does not publish those changes to the board cache. The renderer prefers the cached native observation over the newer durable runtime snapshot for ordinary active states.

```text
durable runtime observation: working
cached host observation: running, turn=null
displayed state: ready
```

This is a concrete possible explanation for the screenshot's “DeLM · Working” alongside “Agent · Ready.” A claim alone should not be used as proof of active execution either.

**Required direction:** publish observations after successful step admission, completion, and resume; give observations a comparable generation/revision so stale data cannot override newer facts. Distinguish native existence, active execution, declared dependency wait, and task ownership.

Sources: [`delm.js:439`](../hosts/claude/hooks/delm.js#L439), [`delm.js:487`](../hosts/claude/hooks/delm.js#L487), [`board-view.js:10`](../hosts/claude/hooks/board-view.js#L10), [`board-render.js:83`](../hosts/claude/hooks/board-render.js#L83). The isolated renderer test at [`board-render.test.mjs:146`](../hosts/claude/tests/board-render.test.mjs#L146) currently expects this precedence without qualifying freshness.

### F08 — Workers cannot discover all shared records on longer runs

**P1 · Both hosts · Source-confirmed**

The model-visible board limits tasks, publications, findings, and check receipts to 24 each. It reports `view_limit` but not collection totals or continuation cursors. `delm_expand` retrieves an exact known ID; it cannot discover an older record the worker has never seen. Human UI pagination is a different interface and does not solve worker discovery.

An older available task can become invisible behind newer task records. Useful shared code, findings, or unchanged verification receipts can also disappear from discovery, encouraging duplicate work or missed reuse.

**Required direction:** keep a compact default, add totals and bounded optional collection queries with stable pagination and useful filters. Preserve exact-ID expansion. Test discovery across more than 24 records, including concurrent updates; do not inject the complete history into every worker turn.

Sources: [`board/mod.rs:24`](../src/board/mod.rs#L24), [`board/mod.rs:1118`](../src/board/mod.rs#L1118), [`board/mod.rs:536`](../src/board/mod.rs#L536), [`reader.rs:393`](../src/board/reader.rs#L393).

### F09 — A normal Codex Python environment can prevent completion

**P1 · Codex · Reproduced**

The completion policy derives external interpreter read authority from the board-transfer filesystem map. That map deliberately narrows access to the private project; it is not the worker's complete native permission policy. In the reproduced unrestricted native configuration it contains only the worker project, while completion has `native_python_runtime=false` and no external runtime roots.

A standard `/usr/bin/python3 -m venv --without-pip .venv` then fails result capture with:

```text
virtual environment interpreter is outside the qualified runtime policy: .venv/bin/python
```

The probe uses the actual `worker_config` and the same result-policy derivation as the run loop. Claude has a different, explicit interpreter capability; the host policies have diverged.

**Required direction:** separate peer file-transfer authority from qualified runtime-dependency inspection. Share environment classification and result rules, with each adapter supplying its real native authority. Preserve explicit denies and symlink containment. A valid worker-local environment should not invalidate source completion; delivery may still omit that environment and require setup in the original project.

Sources: [`run/mod.rs:969`](../src/run/mod.rs#L969), [`workers.rs:1078`](../src/workers.rs#L1078), [`result.rs:24`](../src/workspace/result.rs#L24), [`result.rs:101`](../src/workspace/result.rs#L101). Evidence: `delivery/codex-venv-probe.rs` and its log.

### F10 — Task details describe the wrong kind of dependency

**P2 · Claude UI and shared schema wording · Source-confirmed**

Task `dependencies` are validated as publication IDs, but the Claude detail view labels them “Depends on #…” using task-number formatting. This implies task prerequisites when the underlying records reference shared contributions. Visual fixtures reinforce that confusion.

Task detail also omits useful fields already returned by the reader: interface, completion condition, task kind, and handoff information.

**Required direction:** identify shared-contribution dependencies explicitly and open their contribution details. Show populated task-boundary fields in the detail view. Any actual task-prerequisite feature would be a separate behavior proposal, not a relabeling of the existing field.

Sources: [`board/mod.rs:1223`](../src/board/mod.rs#L1223), [`reader.rs:230`](../src/board/reader.rs#L230), [`board-render.js:335`](../hosts/claude/hooks/board-render.js#L335), [`states.js:11`](../tests/fixtures/claude-board/states.js#L11).

### F11 — Native stop identity needs qualification for descendant types

**P2 · Claude · Conditional risk**

The host tracks descendant agents by `agentId` and supplies that ID to `TaskStop`. Installed native declarations distinguish a teammate's stop address (`teammateId` or name) from its agent ID. If a permitted descendant is such a teammate, the adapter's addressing is insufficient.

**Required direction:** record native task kind, relationship, and stop address separately from identity. Qualify every supported launch path, including background Bash, descendant agents, and any additional background tool offered by the inherited setup. Preserve exact ownership; a name match or working directory must never authorize stopping unrelated work.

Sources: [`delm.js:273`](../hosts/claude/hooks/delm.js#L273), [`delm.js:419`](../hosts/claude/hooks/delm.js#L419), installed Claude `AgentInfo` declarations. This is not established as the tester's background-task failure.

### F12 — Support evidence cannot yet identify the whole failed installation and transition

**P2 · Both hosts · Source-confirmed limitation**

The redacted report is a useful existing foundation: timing, delivery flags, storage counts, and cleanup blockers are explicitly allowlisted. However, its runtime version is the reporting executable's version, not a complete identity of the host, adapter, and runtime that performed the failing run. It also does not expose a structured initiating stop cause and the native acknowledgment state needed to diagnose Q4 without broader private logs.

**Required direction:** retain and export a small safe failure record: original host version, plugin/adapter/runtime identities, run revision, transition cause, selected-result state, exact phase, acknowledgment category, retry outcome, and delivery/cleanup state. Keep prompt text, source, command output, credentials, and raw native payloads out of the default report. Measure first task and first claim alongside first action.

Sources: [`diagnostics.rs:387`](../src/diagnostics.rs#L387), [`diagnostics.rs:719`](../src/diagnostics.rs#L719), [`controller.rs:985`](../src/claude/controller.rs#L985), [`docs/support.md:65`](support.md#inspecting-runs-and-collecting-a-support-report).

### F13 — Saved recovery bytes are not a complete consumer recovery flow

**P1 · Both hosts; maintenance consequence verified for Claude state · Source-confirmed and focused guard reproduction**

Partial recovery stores file bytes as SHA-256-named blobs with a manifest of paths, modes, deletions, and inert symlink targets, then removes worker trees after safe preservation. This is useful storage, but not an immediately usable project. The public CLI exposes inspection/report/cleanup controls without a corresponding consumer command to reconstruct that bundle into editable files. The support guide does not provide a complete restoration procedure.

The common installer also refuses an unresolved recovery state. A probe against its production guard confirmed that a synthetic Claude record with `status=recovery_required`, `finished=true`, and no worker directories still blocks maintenance. The fixture has an empty recovery directory, not a completed recovery manifest or native shutdown evidence. It demonstrates the status gate, not that this synthetic run was safe to update. A tester needing a fixed plugin can encounter an unresolved run, incomplete recovery instructions, and a blocked update.

**Required direction:** provide inspectable recovery contents and an explicit export into a new folder, verifying hashes and modes. Keep applying changes to the original project a separate guarded action. Make retry-delivery, export-partial-work, and uncertain-shutdown outcomes understandable. Permit maintenance only after the relevant ownership/shutdown and preservation conditions have been proved; do not merely add `recovery_required` to an allowed-status list.

Sources: [`delivery.rs:856`](../src/workspace/delivery.rs#L856), [`src/cli.rs`](../src/cli.rs), [`maintenance.mjs:42`](../packages/installer/lib/maintenance.mjs#L42), [support recovery instructions](support.md#results-and-recovery). Evidence: `maintenance-recovery.mjs` and `maintenance-recovery.log`.

### F14 — Candidate stability includes disposable build state

**P2 · Both hosts · Source-confirmed condition; workload reproduction pending**

Completion captures ignored outputs and environment contents, then requires the whole manifest to remain identical after shutdown. A background build or preview can write a log/cache while stopping without changing the source or requested result. Such a change can invalidate the candidate. Large dependency trees also add hashing work even when delivery deliberately omits them.

**Required direction:** use the same explicit accepted source/artifact contract at capture, verification, and delivery. Require those accepted bytes to remain stable. Classify reconstructible environment/cache state separately, while preserving evidence-input fingerprints and shutdown safety. Do not relax validation of a changed requested artifact just because its directory is ignored.

**Qualification:** a controlled server writes an ignored operational log during shutdown while accepted files remain unchanged; delivery should remain valid. A change to a declared output must still invalidate the candidate. Measure hashing cost separately before claiming a speed improvement.

Sources: [`completion.rs:101`](../src/completion.rs#L101), [`completion.rs:150`](../src/completion.rs#L150), [`result.rs:24`](../src/workspace/result.rs#L24).

### F15 — Startup work and execution-allowance wording need alignment

**P2 · Primarily Codex · Source-confirmed sequencing; latency impact unmeasured**

Codex admission includes native metadata/capability inspection, protocol export, project inventory, cloning, and worker capability qualification. Worker preparation/admission contains serial operations. The run's execution deadline is established before native initialization and worker admission, while support says the allowance begins after required startup. The later `expires == 0` check does not reset an already-established deadline.

**Required direction:** define separate preparation/admission and execution budgets, then align code, watchdogs, diagnostics and user wording. Measure cold and warm phases with ordinary installed skills and dependencies. Cache only safe compatibility data keyed by executable identity and runtime version; never cache a live authorization decision merely to improve startup time. Parallelize independent work only after demonstrating that shared resources and failure cleanup remain correct.

Sources: [`run/mod.rs:316`](../src/run/mod.rs#L316), [`run/mod.rs:961`](../src/run/mod.rs#L961), [`run/mod.rs:1037`](../src/run/mod.rs#L1037), [`workers.rs:205`](../src/workers.rs#L205), [`compatibility.rs:389`](../src/compatibility.rs#L389), [`prepare.rs:324`](../src/workspace/prepare.rs#L324), [support](support.md#results-and-recovery).

### F16 — Maintenance protection differs between source and distribution paths

**P2 · Both source-install workflows · Source-confirmed guard difference**

The common installer checks retained/active runs before mutations. The Codex source installer/removal scripts and Claude staged-package replacement do not share that run guard. A build replaces the staged package path after validation; the documented Claude source installation uses that path. Documentation tells users to stop work first, but the primary source-install path does not consistently enforce the protection present in the common installer.

**Required direction:** distinguish building an unused artifact from replacing a package used by an installed session. Apply consistent ownership-aware maintenance checks to activation/removal, and test a normal migration from source to distributed installation. Preserve retained runtime and recovery data across upgrades. Do not block harmless offline builds unnecessarily.

Sources: [`scripts/build.py:150`](../scripts/build.py#L150), [`install_support.py:183`](../scripts/install_support.py#L183), [`install_support.py:215`](../scripts/install_support.py#L215), [`maintenance.mjs:31`](../packages/installer/lib/maintenance.mjs#L31), [source installation support](support.md#retained-state-and-contributor-installations).

### F17 — A failed run blocks ordinary Claude conversation

**Priority:** P0 · Claude · tester-observed and source-confirmed.

Additional tester evidence on October 5 shows a plain request to investigate the failure being treated as another DeLM update. The host immediately repeats the stopping error; the tester reports being unable to use ordinary Claude Code. This is a loss of normal host functionality, beyond an unsuccessful DeLM handoff.

The prompt middleware bypasses only missing/finished runs, plugin-origin input and slash commands. A stopping, disconnected or recovery-required run still intercepts ordinary user text and adds parent control instructions. A rejected update then installs an input failure telling the parent not to act. Repeating normal input repeats the same cycle.

Required behavior: update routing belongs only to a healthy working run. Finalization, cancellation, shutdown failure and recovery must release normal prompt handling. Keep ownership tracking and recovery controls alive independently. If a working-run update fails during a race, report that particular update as undelivered without claiming it was saved, and release later ordinary input. Do not silently execute a failed forwarded task in the parent. Add regressions for repeated prompts after each failure state and for a healthy run that must still forward updates.

The new evidence confirms the user-visible lockout; it still does not identify the event that originally stopped the peers. A second screenshot after restarting and resuming shows the same prompt interception followed by `DeLM is no longer running` and `No such file or directory (os error 2)`. Cover restored sessions with an unavailable control socket as well as failures in the original session. An unavailable runtime endpoint is not evidence that saved work was deleted.

Source: `hosts/claude/hooks/delm.js`, `prompt.submit` and the `inputFailure` handling at the audited revision.

## Broader product review

The following areas were inspected so the plan does not merely patch the screenshots. An existing mechanism is not a claim of universal correctness.

| Area | Current evidence | Required follow-through |
| --- | --- | --- |
| Startup and parent work | Claude uses a short native launch turn; Codex launches through its invocation hook. Both use the same worker policy. | Measure preparation, launch, first action and first claim separately. Preserve immediate independent progress; no parent implementation or board-summary loop. |
| Division of work | Both peers may create, claim, split, release, publish, and integrate. No permanent manager is required. | Inspect actual claims and work intervals before changing prompts. Improve long-run discovery in F08 first. |
| Native setup | Claude forks inherit the session. Codex checks saved capabilities but explicitly lacks exact parity for every live override/connection. | Keep capability evidence and precise support wording. Missing capabilities must not be silently replaced. Qualify ordinary enabled skills/plugins, not only bare-host fixtures. |
| Task ownership | Versioned mutations, claim release, and a single integration claim already exist. | Add event-order and long-queue scenarios; do not infer task count from event IDs. |
| Shared context | Explicit findings/publications are distinct from ordinary private tool output. | Preserve this separation. Add optional discovery; avoid sending raw logs or the board to the parent model for UI updates. |
| Waiting and resumption | Runtime cursors and native resume paths exist. | Exercise producer failure, released tasks, both workers waiting, no-progress repairs, delayed resume, and new user revisions. |
| Checks and receipts | Explicit scoped hashes, observed command evidence, and reusable peer receipts already exist. | Test applicability across imports, edits, environment changes and relocation. Do not require every worker to repeat an unchanged full check. |
| Preview ports | Service claims, OS-assigned ports, ownership checks, and actual listener validation exist. | Exercise launch failure, occupied ports, dead owner, source revision change, separate test contexts and cleanup. Claiming a URL is not proof that a server is healthy. |
| Delivery and artifacts | Guarded source merging, index preservation, conflict journals, and recovery exist. F01 violates artifact delivery. | Add explicit requested-output guarantees before cleanup. Test both hosts through the same delivery contract. |
| Environments | Environment directories can be omitted with a setup/check requirement. F09 rejects a normal Codex environment earlier. | Align qualification and relocation policy while retaining host permission distinctions. |
| Shutdown | Owned processes and native tasks are checked before removal. Claude has the retry/state defects above. | Test natural completion, cancellation, terminal events, late evidence and uncertain ownership as one lifecycle. |
| Recovery | Durable records and partial-change bundles exist; F13 identifies the missing consumer reconstruction route. | Make retry-delivery versus save-partial/export outcomes explicit; verify interruption at each delivery boundary. |
| UI | Real native pane, full-list access, stable detail views and responsive layouts exist. | Correct freshness, IDs, dependency labels and unknown-state wording; test actual runtime events into the renderer. |
| Installation/update | Shared installer delegates to each native manager; F13/F16 identify maintenance and source-path gaps. | Exercise upgrade/reload over saved runs, duplicate scopes and disabled hooks. Report exact installed payload identity. |
| Compatibility | CI includes qualified host versions and latest-host lanes. Native plugin APIs can change. | Add behavioral contract probes, not only package/schema acceptance. Keep compatibility claims tied to observed behavior. |
| Project selection | Current macOS preparation has explicit size, filesystem, and Git-layout limits. | Make rejected inputs clear before model work. Treat worktrees, submodules and additional platforms as separately qualified support, not silent fallbacks. |
| Resource use | UI observation is bounded and separate from model work. Control paths also do synchronous filesystem operations. | Measure large-file/scoped-hash cost and control responsiveness; do not claim a stall without measuring it. Preserve serialized state transitions while moving expensive work only where semantics allow. |
| Diagnostics | Redacted reports exist; F12 leaves key causal evidence unavailable. | Add safe causal and installation identity evidence, with honest missing-data fields. |

### Small presentation issues to include in the same pass

- Unknown native state currently falls back to “Not started” when no task is claimed. Use “Status unavailable” unless startup is actually known not to have happened. Source: [`board-render.js:130`](../hosts/claude/hooks/board-render.js#L130).
- Always show the actual task total. Keep current work prominent and complete history accessible without crowding a small terminal.
- Distinguish an empty queue from a loading, disconnected, or failed observer. Show observed execution even when no task has yet been shared.
- A shutdown warning should identify whether the result is selected, whether changes reached the original project, and what can be done next. “Stopped” must not imply delivery.
- Keep user-facing summaries concise. Put detailed recovery locations and evidence in the existing detail/status surface.

## Why existing verification missed these defects

The current tests provide substantial unit and isolated native coverage, but several boundaries are exercised separately:

1. **The host fixture makes TaskStop successful by default.** This does not model already-finished tasks, refusals, missing IDs, delayed registry state, or a completion notification arriving after failed settlement. Source: [`host.test.mjs:79`](../hosts/claude/tests/host.test.mjs#L79).
2. **The UI fixture seeds already-correct active-turn observations.** It proves native rendering and navigation, but bypasses the production step-to-cache propagation defect. Source: [`claude-board/integration.js:20`](../tests/fixtures/claude-board/integration.js#L20).
3. **Some visual fixtures differ from real board semantics.** Consecutive task numbers and task-like dependency examples do not exercise event-sequence IDs or publication dependencies.
4. **The small real-task qualification deliberately excludes servers and media generation.** This is useful for a fast smoke check, but cannot establish background-shell shutdown or ignored-output delivery. Source: [`verify_claude_native.py:25`](../scripts/verify_claude_native.py#L25).
5. **Codex native lifecycle fixtures qualify ownership signals, not the entire production worker shutdown/delivery path.** This boundary is already documented in [`native-lifecycle-qualification.md`](native-lifecycle-qualification.md).
6. **Passing package validation does not establish runtime behavior.** CI's minimum/latest and architecture lanes are valuable; they still need the failure interleavings that matter here. Source: [CI workflow](../.github/workflows/ci.yml).

The remedy is a small set of better-composed deterministic scenarios, not another long application build for every change.

## Proposed implementation plan

This is the next batch of work, subject to review. Resolve the behavior contracts before editing prompts or changing coordination policy.

### Phase 1 — Establish evidence and lifecycle contracts

Define a shared vocabulary for execution, selected result, shutdown proof, delivery, recovery, and cleanup. Keep native host-specific evidence in adapters, while shared result and delivery invariants live in the runtime.

Persist the transition reason, request revision, selected candidate identity, and settlement generation. Retain actionable failure state across reload. Add the small diagnostics fields in F12 so subsequent failures can be explained without guessing.

The essential invariants are:

- A completed agent turn is not yet a delivered project.
- A stop request is not proof that writers have stopped.
- Unknown shutdown preserves work and produces an actionable state.
- A declared deliverable survives cleanup through verified delivery or durable recovery.
- An accepted update cannot be silently discarded or satisfied by an older candidate.
- Presentation reads facts; it does not control execution or ask a model for progress.

**Acceptance:** table-driven transitions cover complete, cancel, worker failure, deadline, host exit, update race, delivery conflict and storage failure. Duplicate and stale events do not cause a second delivery or erase later work.

### Phase 2 — Make output delivery complete and safe

Implement F01's deliverable selection and F09's environment-policy separation together with tests at the shared delivery boundary. Keep source merging and Git-index preservation intact. Preserve omitted-but-requested output in a recovery bundle whenever automatic delivery is not possible.

Include F14's candidate-stability classification and F13's recovery export in this contract. A saved blob bundle must be reconstructible through a supported action, not only through internal code knowledge.

A delivery report must account for declared source changes and deliverables separately from intentionally omitted environments. Cleanup must be downstream of that accounting, not merely downstream of source-file application.

**Acceptance:** source, ignored MP4/PDF/image outputs, executable files, deletions, binary conflicts, changed ignore rules, virtual environments, unrelated user edits and interrupted delivery each produce the correct project or recovery result. No test may report complete delivery while the only copy of a declared output disappears.

### Phase 3 — Repair Claude finalization and follow-ups

Implement retryable settlement, background-task reconciliation, correct native stop addresses, ordered follow-up admission and recovery of the persistence queue. Keep completion intent distinct from explicit cancellation.

Use the native events and APIs actually qualified for the supported host. Reconcile late terminal evidence automatically; use bounded retry and an explicit attention state when proof remains unavailable. Do not turn an unknown task into a successful stop simply to allow cleanup.

Retain fresh user text before acknowledging it as pending. State exactly whether it reached both peers, is awaiting a supported continuation, or was rejected. Do not let a stale timer stop agents working on a newer accepted revision.

Before retrying delivery, revalidate the retained candidate's revision and accepted outputs after confirmed writer shutdown. Retaining its identity and old check evidence alone does not establish that it remains eligible.

**Acceptance:** the F02/F03/F06 probes no longer reproduce; delayed events and reload converge to the same outcome as the ordinary path. Retry-delivery retains the candidate; intentional cancellation preserves partial work. Both outcomes leave the original project and recovery state truthful.

### Phase 4 — Make coordination and the board understandable

Fix observation freshness, distinct task numbers, counts, publication dependency labels, meaningful detail fields and unknown-state wording. Add optional paginated discovery for workers with bounded responses. Preserve the compact default and existing human full-list access.

Use real Board-generated data for representative visual fixtures. Feed production spawn/step/claim/share/import/complete events through the actual adapter and renderer, instead of seeding the final UI state directly.

**Acceptance:** a working agent stays visibly working until newer evidence changes it; sparse internal IDs do not imply missing tasks; all eligible work and useful records remain discoverable beyond 24 entries; narrow and wide layouts retain counts and navigation; rendering adds no model calls.

### Phase 5 — Qualify both host experiences and installation lifecycle

Run focused shared-contract tests for both adapters. Cover ordinary setup inheritance, prompt boundaries, permissions, background work, original-project delivery and recovery. Add installed-payload identity to the support path and exercise restart/update with retained runs.

Validate native behavior with controlled local fixtures before using a paid task. A small account-backed task is justified only for a remaining host behavior that deterministic or native probes cannot establish; it is not a substitute for reproducing known races.

Apply consistent activation/removal guards to the installed source and distribution routes. Test maintenance eligibility after safe recovery without requiring deletion of retained work.

Update README, support, architecture, release and host documents from the resulting behavior. Describe exact recovery actions and actual support boundaries. Review the installer and package outputs as installed, not only the source tree.

**Acceptance:** package and native tests use the same adapter/runtime being shipped; supported task shapes deliver into the original project; safe cleanup is confirmed; unresolved limitations are specific and visible before users depend on them.

### Phase 6 — Measure speed and refine only the demonstrated bottlenecks

Compare first action/claim latency, control overhead, worker overlap, repeated applicable checks, shutdown delay, delivery delay and time to a usable original-project result. Keep failed runs and recovery cases visible; a faster failure is not a speed improvement.

Resolve the execution-allowance boundary in F15 and record native admission, filesystem inventory, compatibility checks and first-turn admission separately. These are optimization candidates, not measured explanations of this tester's elapsed time.

Worker-turn overlap includes model time, tool time and waits. It is not proof of useful parallel work. Correlate intervals with task ownership, publications, imports and delivered files before claiming good division of labor.

Only then adjust the shared worker instructions where traces demonstrate a specific ambiguity or failure: late claiming, overly broad ownership, redundant integration, or failure to use available peer work. Keep the same instructions for both hosts where mechanics permit. Test separate task shapes rather than optimizing only the tester's video or the earlier game prompt.

**Acceptance:** the corrected system preserves native setup, produces an equally valid delivered result, and does not add parent planning turns, mandatory duplicate verification, or a central coordinator. Publish no numerical speed claim from these synthetic probes.

## Focused qualification matrix

| Scenario | Required outcome |
| --- | --- |
| Initial reconnaissance before claims | Active execution visible; empty queue described truthfully; first claim timing recorded. |
| Sparse task IDs and more than 24 records | Stable identity, clear total, complete bounded discovery for workers and users. |
| Claim/release/split/import interleavings | Ownership versions remain correct; stale mutations fail clearly; unrelated work continues. |
| Candidate followed immediately by an update | Exactly one valid admission order; no accepted message lost; old finalizer cannot stop a new revision. |
| Natural shell completion before/after a stop snapshot | Native evidence reconciled; no indefinite failed handoff after proof arrives. |
| TaskStop refusal, missing task, or delayed registry | No invented shutdown; retry or actionable preservation state. |
| One worker fails; both workers cannot continue | Surviving work continues where possible; otherwise clear initiating reason and saved-work outcome. |
| Explicit stop versus retry delivery | Different, predictable results; completed candidate retained for a retry. |
| Store write fails once | Error visible; ordering preserved; later safe writes/recovery possible. |
| Ordinary prompt after stop failure or recovery | Normal Claude handling resumes; no repeated forwarding error, parent prohibition or false update acknowledgment. |
| Bridge stream loss or host reload during finalization | Durable intent survives; no duplicate application, orphaned ownership or silent deletion. |
| Ignored requested output | Delivered or preserved explicitly before cleanup. |
| Python environment and native permissions | Valid environments accepted under actual authority; denied external paths remain denied. |
| Port conflict, preview owner death, mutable test context | No unrelated process stopped; one correct service generation; invalidated checks not reused. |
| Original project edited during delivery | Compatible changes merged; conflicts retained; index and unrelated edits preserved. |
| Scope-limited check reused after an import | Matching evidence reusable; affected changes invalidate it; no mandatory repeat full suite. |
| Narrow terminal, disconnected view, long text | Legible state/counts/actions; unknown state not displayed as success or non-startup. |
| Plugin update/remove during active or retained run | Clear supported action and retained ownership; no reliance on an unrelated replacement binary. |
| Safely preserved work awaiting manual reconciliation | Supported export into a usable folder; maintenance rules based on proven safety, not an unexplained permanent block. |
| Cache/log changes during shutdown | Disposable state does not invalidate unchanged accepted output; real output changes still do. |

Keep each deterministic scenario small. Run the relevant subset while fixing a boundary, then the normal repository verification and installed-payload checks before a release candidate. Repeat tests when a change or unresolved failure warrants it, not simply to accumulate test counts.

## Evidence needed to close the original incident

The current code reproductions justify the work above. They do not establish the tester's original causal sequence. Obtain the smallest relevant record set:

1. Actual host/plugin/runtime identities and the run ID.
2. The initiating `claude_cancelled` event or the no-active-worker transition, with its reason.
3. Each peer's completion declaration and native turn completion reason.
4. The structured TaskStop outcome and subsequent native background-task observations for the named task.
5. The final `claude.json` phase plus delivery result/recovery state, excluding credentials and full task text.
6. Task creation/claim events and true collection counts for the empty/missing-task questions.

This can distinguish cancellation from failed candidate capture, an already-ended background task from a still-active one, and actual missing task creation from misleading UI. Do not request entire private transcripts by default or manipulate the tester's preserved workspace merely to make the run appear complete.

## Research sources and qualification boundaries

- Local source at the audited revision is the primary evidence for DeLM behavior. Code references above are line anchors for that revision.
- Context7 was queried for official Claude Code and OpenAI Codex documentation. Generic Agent SDK/TaskStop documentation was not treated as proof of native plugin event ordering.
- [Claude native fork documentation](https://code.claude.com/docs/en/sub-agents#fork-the-current-conversation) describes inherited conversation, model, system prompt and tools. It does not establish DeLM's complete lifecycle correctness.
- [Claude hook reference](https://code.claude.com/docs/en/hooks#subagentstop) describes background-task snapshots as in-flight work scoped to the parent session. This supports interpreting a snapshot carefully; it is not a replacement for owned-process shutdown checks.
- Installed Claude Code `2.1.289` generated declarations were inspected for `AgentInfo`, `AgentStatus`, `SubagentStop`, native turn completion and task notifications. The public [native plugin declarations](https://github.com/anthropics/claude-code/blob/main/mods/types/claude-code.d.ts) retrieved during research identified version `2.1.277` and describe the surface as early access. Installed-version declarations take precedence for this local audit.
- Official [Codex API documentation](https://github.com/openai/codex/blob/main/sdk/python/docs/api-reference.md) exposes fork configuration and control APIs. It does not guarantee all live in-memory settings transfer into an independent app-server process. DeLM's [native inheritance tests](../tests/native_inheritance.rs) and documented gap remain the relevant local evidence.

## Local reproduction record

The ignored audit directory contains the existing-suite log, isolated reproduction sources/results, and source provenance where recorded. These artifacts contain synthetic projects, not the tester's private run.

| Evidence | What it establishes |
| --- | --- |
| `claude-existing-tests.log` | Existing 97 JavaScript tests pass at the audited source. |
| `delivery/ignored-output-probe.rs` and log | Real shared delivery excludes an ignored requested artifact and removes its worker copy. |
| `delivery/codex-venv-probe.rs`, `delivery/codex-venv.log`, `delivery/provenance.json` | Real worker policy derivation rejects a standard external Python interpreter link; provenance binds sources and compiled library. |
| `lifecycle/` | Isolated host-event reproductions for late shutdown evidence, candidate/update ordering, and transient persistence failure. |
| `board/stale-native-state.mjs` and `board/stale-native-state.json` | Renderer reproduction of stale host observations overriding newer durable working state. |
| `board/task-pagination-existing-test.log` | Existing real-Board test passes with 40 tasks: human pages expose 32 + 8 while the worker view contains only 24. |
| `maintenance-recovery.mjs` and log | Production maintenance guard refuses a finished recovery-required state without remaining worker directories. |

Implementation was authorized after the investigation. The verification record should be read alongside this historical evidence: product fixes include focused regressions and user-facing recovery behavior across both integrations.
