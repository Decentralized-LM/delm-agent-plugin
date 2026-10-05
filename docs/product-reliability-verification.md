# Product reliability implementation and verification

Date: 2026-10-05. Branch: `fix/plugin-reliability`.

This records the changes following the [reliability investigation](product-reliability-research.md). The investigation describes the starting revision; this document describes the implementation and its verification boundaries. Local reproduction logs remain in ignored `.validation/` directories. No tester project or account was modified.

## User-visible outcomes

- A finishing, failed, restored, or unreachable Claude run releases ordinary conversation input. Recovery does not require giving up the conversation.
- A failed shutdown retains the selected result and offers **Retry finishing**. Explicit cancellation saves partial changes instead. These actions have different, recorded intentions.
- Requested generated files, including ignored video and report outputs, participate in the same capture, verification, and delivery contract as source changes.
- Source-only results explicitly declare no requested artifacts. Additional incidental output is preserved for review without misreporting a fully declared successful delivery as a failure.
- Saved partial changes can be inspected and exported to a new folder with `delm recover`. The command verifies content and project identity and does not overwrite the original project.
- The board reports observed activity accurately, uses consecutive display numbers for tasks, and explains publication dependencies. Workers can discover records beyond the compact view.
- Source activation and distribution maintenance protect unfinished runs. A fully saved, verified recovery with confirmed shutdown does not block maintenance indefinitely.

## Lifecycle and conversation ownership

The controller records finalization generation, request revision, intent, reason, attempts, and shutdown acknowledgment. Beginning settlement closes update admission for that generation. An update accepted first invalidates an older finalizer; an update rejected after the boundary is not described as delivered.

Native shutdown stops recorded background shell tasks before their owning agents. This avoids the reported sequence where stopping an owner retires its child before the child's separate stop can acknowledge termination. Structured native task notifications can reconcile late terminal evidence for exact owned tasks. Generic not-found responses and arbitrary output text do not establish shutdown. Agent terminal status, owned services, and the scoped operating-system writer check still gate delivery and cleanup.

Settlement is single-flight and retries are bounded. Later native proof can trigger another attempt, and the board provides explicit retry. A persistence failure does not permanently reject every later store write. Retried delivery reconciles durable delivery records, including when workspace cleanup succeeded before the final host-state write; it does not reapply changes over later user edits.

Conversation input ownership is independent of retained workspace ownership. Recovery work is scheduled separately from ordinary prompt handling. The parent neither repeats failed forwarding forever nor waits on an obsolete update after the run loses input ownership. An explicit failed update receives a truthful failure response; it is not silently executed as a new parent task.

## Parallel work and latency

The common worker instructions require useful independent work, reasonably sized claims, early contributions, and splitting a long task when another peer can help. They prohibit shell sleeps and background loops for peer dependencies. A peer with no useful ready work declares a named dependency and ends its turn; relevant board events resume its existing identity.

Necessary timed polling is capped at 30 seconds in worker policy, with event-driven completion preferred. This is an instruction to workers, not a claim that arbitrary project code cannot contain a longer sleep. Runtime polling is substantially shorter. RPC timeouts and the overall run deadline are bounds on active operations, not idle polling intervals.

An accepted completion declaration temporarily holds pending resume controls while its native turn closes. It does not stop active peers or count as a delivered result. Failure, withdrawal, and newer revisions release the hold through events; a selected result makes stale resume controls obsolete. This covers the report's completion-declaration-to-parent-resume race.

Claude now gates each peer's first step on that peer's own binding. One slow binding does not hold up an already configured peer. Candidate admission still requires the identities needed for safe finalization.

Independent component checks can overlap with implementation. The assembler reuses applicable scoped receipts. There is no new central planner, mandatory second complete test pass, or verifier approval gate.

### Measured capture improvement

A controlled local debug-build fixture contained 128 MiB of ignored dependencies and operational caches, plus source and a requested ignored artifact. Three measurements compared whole-tree capture with the accepted source/artifact selection:

| Measurement | Whole-tree capture | Selected capture |
| --- | ---: | ---: |
| Median elapsed time | 3285.399 ms | 57.984 ms |
| Regular-file bytes included in hashing | 134,217,805 | 77 |

The declared artifact was delivered and worker directories were removed. The original ignored-output reproduction now preserves its undeclared output for recovery instead of silently deleting it. The measurement establishes a local capture improvement only; it is not an end-to-end task speedup or a quality comparison.

Evidence: `.validation/plugin-reliability-implementation/delivery/` contains the benchmark source, logs, source snapshot, and runtime/source hashes.

## Delivery and recovery contract

Capture, post-shutdown verification, and delivery use one selected source/artifact manifest. Changes to accepted output still invalidate completion. Disposable ignored caches and recognized dependency environments are omitted before hashing. Tracked or baseline source remains protected, and requested output inside a cache-like path is retained. Arbitrary external source links and credentials remain excluded.

An explicit `artifacts` field, including `[]`, accounts for requested outputs. Omission stays distinguishable for older or incomplete declarations. Additional custom files are preserved from both workers before cleanup. This avoids both losing generated deliverables and treating every incidental build file as a required deliverable.

Recovery export is a delta, not a reconstructed full project: `files/` contains changed content, `base/` contains available prior content, and `manifest.json` records deletions, symlink targets, and metadata without executing them. It verifies blobs and requires a new destination outside the original project and recovery storage. The selected run's project identity must match the bundle.

The source installer rechecks active-run state before native mutations, including after a long build. In-place Claude adapter replacement is guarded separately from harmless offline staging. Both maintenance routes validate recovery structure and blob hashes, refuse workspace remnants and contradictory shutdown acknowledgments, and preserve recovery evidence.

## Findings addressed

| Investigation | Implementation |
| --- | --- |
| F01, F14 | Shared source/artifact selection, explicit artifact accounting, pre-hash cache exclusion, retained additional output. |
| F02–F06, F17 | Durable finalization, revision admission boundary, retryable shutdown, intent-preserving recovery, recoverable persistence queue, ordinary-input release. |
| F07 | Durable/native state outranks stale observation cache; unknown state remains explicit. |
| F08 | Bounded `delm_list` discovery, counts, cursors, and task filters with stale-page checks. |
| F09 | Ordinary omitted virtual environments avoid external interpreter inspection; legacy validation uses actual native policy. |
| F10 | Stable human task numbers, publication dependency labels and details, clearer schema limits. |
| F11 | Exact native descendant stop addressing, children-first shutdown, structured terminal evidence, writer fence. |
| F12 | Original runtime/host version and allowlisted lifecycle/artifact metadata in support reports. |
| F13 | Verified partial-change inspection/export and safe maintenance after completed recovery. |
| F15 | Correct execution-allowance wording, removal of the shared Claude binding gate, measured capture optimization. Broader startup and task-level performance still require measurements. |
| F16 | Source activation/removal and distribution maintenance guard the same unfinished-work boundary. |

## Verification

The integrated `./scripts/verify.sh` run passed. After the final Claude cancellation and native callback changes, the affected JavaScript suites and native package validation were rerun successfully; formatting and diff checks also passed.

| Check | Result |
| --- | --- |
| Rust formatting, all-target Clippy, build | Passed |
| Rust unit/integration tests | 275 passed; 4 existing test entries ignored by default |
| Python tooling tests | 101 passed |
| Installer JavaScript tests | 64 passed |
| Final Claude JavaScript tests | 129 passed |
| Native Claude package validation | Strict validation passed on 2.1.289, using the staged final adapter and built runtime |
| Bundled runtime MCP and observer interface probes | Passed without model calls |
| Final diff whitespace check | Passed |

The strict native check caught and corrected an unsupported dynamic callback that mock tests had accepted. The final retry action uses a session-bound closure owned by the host module. Its tests verify duplicate-click suppression, conversation changes, and that no native API context is forwarded through the callback.

Evidence: `.validation/product-fixes-2026-10-05/verify.log`, `claude-final.log`, `native-validation.json`, and `source-manifest.json`. The four ignored entries comprise two explicitly configured native checks, a manual preparation-timing probe, and a subprocess fixture invoked by its supervision tests. Ignored entries were not counted as passes.

Focused regressions cover:

- An active background child during completion, late shutdown evidence, and a rejected or mismatched stop acknowledgment.
- Completion versus newer input, declaration versus queued resume, failed completion release, and one slow peer binding.
- Failed persistence, dead bridge, restored session, ordinary prompts after failure, and delayed callbacks from another conversation.
- Requested ignored outputs, source-only completion, incidental output retention, changed accepted files, cancellation, and repeated delivery after cleanup.
- Recovery corruption, wrong project identity, destination collision, inert symlinks/deletions, and preservation of later original edits.
- Long collection discovery, stale filtered cursors, accurate active-worker state, and task/publication identity distinctions.
- Activation after a run starts during compilation, malformed recovery state, contradictory shutdown proof, and preserved recovery bytes.

### Native board presentation

The production board renderer was exercised inside Claude Code 2.1.289 with deterministic sample data and isolated configuration, without model calls:

- **140×40:** working agents, task rows, shared contribution/import, recovery, conflict, and completion.
- **80×24:** recovery actions and details remain reachable; Page Down/Up, hide/reopen, and draft-input preservation were checked.
- **3/3 native interaction tests passed.** The final board suites also passed 71 JavaScript tests, 39 Rust board tests, and 10 Rust view tests.

Evidence: `.validation/plugin-reliability-board/native-wide-final/`, `native-narrow-final/`, and `final-source-manifest.json`. Images are terminal cell renderings of captured native ANSI output using the production renderer; the data is synthetic. They do not depict a new model-backed task.

### Qualification boundaries

The tester's timeline was supplied as a report. Its private raw run was not available locally. Deterministic regressions reproduce the relevant state transitions but do not certify every native host ordering. The new shutdown sequence and task-notification reconciliation still need confirmation in a real affected native run before claiming the original incident is fully closed.

No new paid full-workflow run, Intel-machine qualification, listening review, or public release certification is implied. The current changes do not establish exact live-session capability parity for Codex. Support reports identify recorded versions, but versions alone do not identify every installed byte. These boundaries remain visible in the support and release documentation.

Remaining optimization candidates include sequential Codex fork/admission acknowledgments and large publication/import file operations in serialized coordination paths. They need measurements and ordering/permission checks before redesign; this change does not assert that every task now runs faster.
