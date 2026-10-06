# DeLM product and runtime plan

Status: the core implementation and Claude terminal board are implemented. This document records their design boundaries and acceptance criteria; it is not a statement that a public release has been qualified or published. The agent-visible proposals in the review checkpoint below remain separate decisions.

The goal is to make DeLM reliable, fast, and straightforward to use in Codex and Claude Code while preserving its collaboration model. The [terminal UI plan](terminal-ui-plan.md) documents the implemented Claude board. Current installation and release work is tracked in the [release guide](releases.md).

## Design boundaries

- Workers perform the task and coordinate through the shared board. The parent remains a lightweight control and delivery channel.
- Preserve the user's native skills, tools, model settings, instructions, and permissions. Identify unsupported cases precisely.
- Keep task ownership, deliberate sharing, contribution reuse, and completion rules consistent across hosts.
- Preserve evidence validity, safe delivery into the original project, and recoverability while optimizing runtime overhead.
- Review changes to worker prompts or model-visible responses explicitly before implementing them.

## 1. Correct Claude conversation and input handling

Bind each run and every control operation to the correct native conversation. Handle clearing, switching, branching, and returning to a conversation without retaining another conversation's active run.

Preserve the inputs needed for follow-up instructions and explicitly reject inputs the native API cannot forward. Accepted text and additional text context are forwarded together; initial native forks inherit their conversation. The [supported boundaries](#supported-boundaries) below explain the attachment and reference limitations.

Acceptance criteria:

- Instructions, status, cancellation, and final delivery remain associated with the correct conversation.
- Returning to a conversation restores supported control or clearly explains the required recovery action.
- Both existing workers receive complete updates under the correct request revision, once each.
- Workers cannot acknowledge an update before its required inputs arrive.
- Unavailable inputs produce a specific error rather than an apparent successful delivery.

Primary implementation: [Claude lifecycle hooks](../hosts/claude/hooks/delm.js) and [Claude controller](../src/claude/controller.rs).

## 2. Fix missed worker wakeups

Include newly created tasks among the events that can wake an eligible waiting Codex worker. On both hosts, reconcile readiness when a worker enters waiting so that useful changes arriving just before its native turn ends are not missed.

Acceptance criteria:

- Ready work is used regardless of whether its event arrives before, during, or after the worker enters waiting.
- Multiple relevant events produce one pending resumption of the existing worker.
- Resumption preserves native identity, conversation, permissions, and the current request revision.
- Cancellation, accepted completion, and genuinely user-blocked work prevent inappropriate wakeups.
- Unrelated changes do not repeatedly wake a worker whose dependency remains unresolved.
- Coordination does not require periodic model polling or repeated completed verification.

Primary implementation: [Codex runtime](../src/run/mod.rs) and [Claude controller](../src/claude/controller.rs).

## 3. Preserve the user's normal setup

Strengthen coverage for inherited skills, tools, instructions, model settings, and permissions. Investigate the remaining differences between saved configuration and the active session, including command-line overrides and live tool connections.

Acceptance criteria:

- Existing Codex setup comparisons remain effective, including enabled skills, instruction dependencies, MCP tools, and authentication state.
- Claude continues to use native conversation forks with the inherited setup.
- Supported settings are preserved, and unsupported cases receive an accurate explanation.
- Equivalent host-independent behavior is exercised against both adapters.

Primary implementation: [Codex worker setup](../src/workers.rs), [Claude hooks](../hosts/claude/hooks/delm.js), and the [shared worker policy](../plugin/worker.md).

## 4. Reduce demonstrated runtime overhead

Measure preparation, worker admission, first useful work, active overlap, waiting, handoff, delivery, and cleanup separately. Measure coordination-response size and repeated scoped checks where the available evidence supports those conclusions.

Use those measurements to select optimizations. Candidates include redundant host initialization, avoidable sequential admission, repeated state serialization, and synchronous file work that delays control handling. A candidate becomes an implementation task only when measurement supports it.

Acceptance criteria:

- Reports distinguish runtime overhead from model execution and support comparison across both hosts.
- Instrumentation adds no mandatory model turn or verification gate.
- Each optimization improves the relevant phase or responsiveness while preserving permissions, ownership, durable recovery, and evidence validity.
- Speed claims reflect measured results rather than assumptions about parallelism.

## 5. Improve installation and maintenance

Make readiness checks and installation errors actionable. Check update and removal behavior during active runs, and identify unsupported project layouts before expensive preparation where possible.

Acceptance criteria:

- Users can distinguish an installed package from a usable host integration.
- Errors identify the failed prerequisite and a concrete next action.
- Maintenance preserves active-run control and recoverability.
- Project compatibility failures explain the relevant constraint without modifying the source project.

## 6. Make recovery and retained data manageable

Distinguish active runs, completed runs, and unresolved recovery. Provide understandable storage information, deliberate cleanup, and a diagnostic report that excludes private content by default.

Acceptance criteria:

- Users can determine whether changes reached the original project and whether temporary workspace cleanup completed.
- Any required original-project setup or focused check is identified accurately.
- Cleanup preserves active runtime resources and unresolved recovery material.
- Diagnostic exports exclude source, prompts, command output, and recovery contents unless explicitly selected.
- Detailed local evidence remains available for investigating failures.

## 7. Finish documentation and focused regression coverage

Document shared behavior once and make host-specific differences explicit. Keep installation, maintenance, support, and architecture documentation consistent with the actual product.

Acceptance criteria:

- Both hosts have clear installation and usage instructions.
- Shared behavior remains in shared modules where practical; native protocol handling stays in its host adapter.
- Focused tests cover conversation ownership, complete input delivery, wakeup ordering, cancellation, and affected delivery or recovery behavior.
- Verification targets the changed behavior and material integration risks without repeating unrelated full workflows.
- Public-facing prose is concise, accurate, and consistent across the repository.

## Review checkpoint: information received by agents

These are separate proposals, not approved implementation tasks. Before changing any of them, present a concrete current/proposed response example, explain who receives it, and identify the effect on model context and evidence access.

The current shared board contains tasks, findings, published contributions, worker status, and explicitly shared check records. It does not automatically contain every shell command. The `recent_commands` field is separate metadata returned to the worker making a coordination call. Claude currently returns that worker's commands for the current request revision; Codex returns its most recent 12.

### A. Worker command metadata

Consider avoiding repeated, expanding command metadata while preserving access to native command IDs needed for verification evidence. Compare a bounded response with explicit access to older evidence against the existing behavior. Preserve command identity and receipt validation.

### B. Board discoverability

The default board view limits each collection to 24 entries. Consider explicit overflow information and retrieval of older entries so that work remains discoverable. Preserve task ownership, versions, references, and completion rules. Review the model-visible interface before implementation.

### C. Parent completion report

Consider giving the parent a concise outcome, delivery status, meaningful verification summary, and remaining issues, with detailed evidence available separately. Current final handoffs contain selected check evidence; Codex can include native command output for those selected checks. This is distinct from continuous sharing of command history.

The parent already performs host control and final reporting, and may perform narrowly scoped original-project checks when delivery requires them. Reducing its input should preserve the information needed to report results truthfully.

## Implementation sequence

1. Correct conversation/input handling and worker wakeups, with focused regression coverage.
2. Verify setup inheritance and collect runtime phase measurements.
3. Implement optimizations supported by those measurements, then complete installation and recovery improvements.
4. Reconcile documentation and validate the affected behavior across both hosts.

Resolve the agent-information proposals independently before scheduling their implementation. The Claude terminal board is implemented separately from those model-context proposals.

## Implementation record

| Area | Delivered behavior and evidence |
| --- | --- |
| Conversation and inputs | Claude state is keyed by conversation, including delayed events and concurrent starts. Accepted text and additional context are forwarded together. Tests cover switches, recovery, failed delivery, native refusals, and unchanged fork settings. |
| Worker wakeups | Both adapters reconcile readiness after native turn completion. Codex also wakes for task creation. Board, controller, and scripted native-host tests cover event order, dependency relevance, duplicate events, and preserved worker identity. |
| Setup inheritance | Added regression cases for skill enablement, dependencies, plugin identity, MCP authentication, and discovery failure. Native fixtures cover saved configuration, metadata lookup, and production Codex startup without remote model calls. CI checks configured versions and latest releases; support still depends on passing evidence for the actual host version. |
| Runtime overhead | Added passive phase, worker-turn, waiting, and coordination-response measurements. Observations add no model calls or per-sample filesystem synchronization. The wakeup fixes remove missed-event idle time; cheap layout checks reject unsupported projects before recursive capture. Other performance candidates remain dependent on measurements. |
| Installation | Structured and readable status distinguish enabled installation from unverified session readiness. Mutation preflights preserve active and uncertain runs, including failed startup records, with actionable errors. |
| Recovery and diagnostics | `delm runs`, `delm report`, and explicit preview/confirm `delm clean` expose run state, timing, and eligible diagnostic storage. Cleanup retains ownership, completion evidence, shared board, delivery records, recovery contents, and original-project files. |
| Documentation | README, support, development, architecture, Claude integration, and both source and prepared-installer instructions describe the supported behavior and its limits. |
| Claude terminal board | The native board displays agents, task ownership, shared context, and delivery or recovery state. It opens with a run; hiding or reopening it does not start model work. See the [terminal UI plan](terminal-ui-plan.md). |

### Supported boundaries

- Claude's current native API permits text append into existing forks, but does not expose attachment contents for forwarding. New media or unresolved references in follow-ups receive a specific refusal before the task revision changes. Initial forks inherit attached context; users can paste text or stop and relaunch with an attachment. This satisfies the explicit-error requirement without silently dropping inputs.
- Codex saved configuration and native fork settings are checked. Parent-process CLI overrides and every live integration are not exported by the current host API; exact live-session parity remains explicitly unverified.
- Installer maintenance checks are preflights, not locks against a new run starting concurrently. Users must keep DeLM stopped while updating or removing it.
- Diagnostic sizes are logical file sizes, not APFS physical-space estimates. Timing reports distinguish unavailable evidence from measured zero and do not establish a speedup by themselves.

### Verification

Run from the repository root:

```sh
./scripts/verify.sh
npm --prefix packages/installer run test:native
npm --prefix packages/installer run test:native:claude
DELM_TEST_HOST="$(command -v codex)" cargo test --locked --test native_inheritance -- --ignored
./scripts/build.sh --host all
```

These checks use deterministic fixtures, disposable native host configurations, and package validation. They do not start account-backed model tasks. Successful local checks do not establish release signing, publication, or the result of remote CI.
