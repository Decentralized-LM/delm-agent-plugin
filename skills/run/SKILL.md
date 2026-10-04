---
name: run
description: Build in parallel with collaborating Codex agents and deliver their changes to your project.
---

Use DeLM only when explicitly invoked. The trusted UserPromptSubmit hook captures the exact request and starts the runtime. Its developer context supplies an absolute `follow --capture` command. Execute that command immediately through the normal execution tool, retaining its process handle when it yields. Do not reconstruct the task, inspect the project first, generate launch files, send a separate startup confirmation, or launch a second run.

If that capture context is missing, explain that DeLM's trusted invocation hook did not run. Ask the user to review DeLM in `/hooks` and restart Codex. Do not invent a capture path or bypass native ownership. An invocation applies to the current project root; resolve an ambiguous project selection before asking the user to invoke it there.

Native worker forks carry the preceding conversation and captured user input. DeLM adds coordination instructions without removing ordinary saved skills, plugins, hooks, or MCP configuration. Runtime capability reports identify any unverified live-session inheritance; never claim exact setup parity that the report does not establish. Surface startup/input errors instead of silently dropping references or replacing them with your own summary.

Read the event stream and give concise updates when meaningful progress appears. Keep following the original process; do not create a polling loop of repeated status commands. Use the reported `control_executable` and `run_id` for subsequent control operations. Do not implement the task or edit the original project while the workers run.

Forward steering with `update --run-id <id> --message-file <UTF-8-file>`, adding `--inputs-file` for newly selected local references. On a stop request, immediately use `stop --run-id <id>`, then collect the outcome.

Relay native questions through the host's clarification flow, then send the user's answers with `answer --run-id <id> --question-id <request-id> --answers-file <JSON-file>`. For native approvals, use `respond --run-id <id> --request-id <request-id> --response-file <JSON-file>` with the response envelope required by the reported native method. Honor existing explicit authorization when applicable; otherwise obtain the user's actual decision. Never invent approval or authentication proof. An ordinary update resolves neither a pending question nor an approval.

Completion applies changes to the original project and removes both temporary workspaces after confirmed process shutdown. Read the final delivery report. `delivery_conflict` means delivery is incomplete; explain the conflict without overwriting user edits. A recovery path can also preserve original-file preimages after successful delivery; check `delivered`. A stopped or failed run contains partial work, not a completed result.

For `delivered` with `verification_required`, perform only the necessary native setup or focused check in the original project, guided by `merged_paths`, `environment_files_changed`, and `environment_directories_omitted`. Preserve native permissions. Do not copy worker environments or automatically repeat the full test suite. Report ready only after those checks succeed.

Link the original project, summarize actual changes and verification, and disclose any remaining limitation. Temporary previews stop with the run. Start a requested preview from the original project before linking it; do not present a stopped worker URL as live.
