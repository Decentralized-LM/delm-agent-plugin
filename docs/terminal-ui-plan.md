# Terminal experience plan

Status: planned for after the core runtime fixes. This document records the proposed work; it does not authorize implementation.

The [product and runtime plan](product-runtime-plan.md) covers execution, reliability, installation, and recovery. This separate plan covers what the user sees while DeLM runs in Claude Code or Codex.

## Purpose

Make DeLM's collaboration easy to understand through a clear, polished terminal view. Users should be able to see how agents divide work, share useful contributions, and deliver a result without reading a stream of internal commands.

The view observes the runtime. It does not direct agents, choose tasks, change shared context, or introduce another model into the workflow.

## Scope

The default view presents:

- Task ownership and meaningful task states.
- Each agent's current activity, where the runtime provides reliable information.
- Contributions and findings that agents explicitly share.
- Questions, permission requests, or other events requiring user attention.
- Delivery, cleanup, and recovery status.

Display updates come directly from runtime events through ordinary application code. Updating the screen must not require model calls, generated narration, or additional conversation messages.

Claude Code and Codex should communicate the same essential information using interfaces supported by each host. The exact display capabilities need qualification before choosing a design; identical pane layouts are not assumed.

## Implementation steps

### 1. Establish the event-to-display contract

Map existing runtime events to the information the user needs. Distinguish events that report an action from events that confirm its result, so the display cannot imply that a task was claimed, a contribution integrated, or delivery completed before it actually happened.

Document any missing event information before changing the runtime. Display requirements must not quietly change what agents receive or how they collaborate.

### 2. Design a compact default view

Create a clear hierarchy for active agents, task ownership, shared contributions, and user attention. Favor stable placement, readable text, and purposeful changes over repeated log lines.

Show enough information to explain progress without reproducing every command or presenting unknown activity as established fact.

### 3. Integrate with supported host interfaces

Qualify Claude Code and Codex display surfaces, then implement the appropriate presentation for each. Preserve each host's normal input and interaction flow.

Keep rendering separate from the model conversation and coordination tools. The same underlying events should produce consistent facts across both integrations even if their layouts differ.

### 4. Make additional detail optional

Provide access to task details, shared findings, verification evidence, and recovery information when needed. Keep the default view concise, and distinguish shared information from a worker's private command history.

Choose the interaction after confirming each host's supported capabilities. Avoid adding routine command dumps or repetitive progress messages to the conversation.

### 5. Make completion and interruption unambiguous

Show whether changes reached the original project, temporary workspace cleanup completed, or recovery needs attention. Handle waiting, cancellation, failure, and completion as distinct states.

Review the full flow for legibility, accurate event ordering, and smooth updates under both quiet and busy workloads.

## Acceptance criteria

- A user can identify which agent owns a task and understand the latest meaningful progress without opening raw logs.
- Shared contributions appear only when supported by runtime events.
- Screen updates invoke no models and add no display-only messages to agent conversations.
- Rendering neither changes task scheduling nor blocks workers from making progress.
- Repeated or delayed events do not create duplicate activity or misleading state transitions.
- Both hosts preserve normal input, questions, and permission interactions.
- Details remain available without overwhelming the default view.
- Delivery, cleanup, and recovery states match the actual result.

## Dependencies and review

Begin this work after the main plan's conversation handling and worker wakeup fixes establish reliable lifecycle events. Confirm the available host display interfaces before committing to a layout.

Review a proposed layout and its event mapping before implementation. Any change to agent-visible information belongs in the main plan's separate review checkpoint, not in this UI work.
