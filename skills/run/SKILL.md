---
name: run
description: Run an explicitly requested task with two isolated Codex workers sharing progress through DeLM.
---

Use the bundled runtime at `../../bin/delm`, resolved relative to this skill directory. It starts two stock Codex workers in private project copies and retains their results.

Select the Git repository requested by the user, using its exact root. If the current folder contains several repositories and the request does not identify one, ask which project to use. Never pass a collection folder or copy projects yourself. Do not edit the original project or implement the task in the parent conversation while DeLM runs.

Generate a fresh invocation UUID with `uuidgen`. Write the user's task verbatim to a temporary UTF-8 file, removing only the leading skill invocation. Do not rewrite it as a plan or add requirements, a worker split, or your own implementation preferences. Put relevant earlier constraints, decisions, and file references in a separate context file, clearly distinguishing user decisions from your observations. Workers do not inherit the chat automatically. Start the runtime as a foreground command through the native execution tool:

```text
exec '<plugin-root>/bin/delm' run --launch-token <uuid> --project '<absolute-project-path>' --task-file <absolute-task-file> [--context-file <absolute-context-file>] [--inputs-file <absolute-inputs-json>] [--seconds 1800]
```

Carry user-selected images and reference files through `--inputs-file`, using a temporary JSON manifest:

```json
{"version":1,"files":[{"kind":"image","path":"/absolute/screenshot.png"},{"kind":"file","path":"/absolute/specification.pdf"}]}
```

Include only files the user selected or explicitly referenced for this task, including required ignored assets. Existing project files already admitted into the private snapshot need no extra copy. Images are sent as native image input; other files become read-only captured references. Each batch supports 16 regular files, at most 32 MiB each and 128 MiB total. Do not include credentials, directories, or a session-only attachment ID. If a selected attachment has no accessible local file, explain that it cannot yet be forwarded and ask for an accessible file before starting; do not silently drop or replace it with a guessed description.

Use the exact installed executable with the single-command `exec` form above; native hooks bind this launch to the invoking chat. If the handshake is unavailable, ask the user to review DeLM in `/hooks` and restart Codex. Never bypass the handshake by unsetting native identity variables, using `--stdio`, or starting an unbound background run. If a launch has consumed its handshake and then failed, explain the error and ask for a fresh user message before retrying. A new UUID in the same turn cannot safely distinguish a retry from a delayed interruption.

Use `--model MODEL` or `--effort EFFORT` only for an explicitly requested override. Keep the execution tool's process handle when the command yields. Read the JSONL progress events; the settings event supplies `control_executable`, and the first run events identify the `run_id`. Use that absolute control executable for all subsequent status, update, and stop commands. It remains available for status and recovery if the installed plugin is updated or removed. Such a change stops the current run and preserves its work.

As soon as a run ID appears, call the following command to confirm control access. Workers will not begin task model turns until this succeeds. Resolve any native permission request before continuing; do not launch a replacement run while this one is waiting.

```text
<control_executable> status --run-id <uuid> --keep-alive --wait-seconds 10
```

After control confirmation, use `status --run-id <uuid> --after <update_sequence> --wait-seconds 15`, passing the `update_sequence` from the last status response, and collect output from the original execution handle. Each call waits for new progress or a question, returning within 15 seconds even when nothing changes. Always advance the cursor from the response, including while a question awaits the user, to avoid repeatedly fetching the same update. Native lifecycle events govern cancellation; continued work does not depend on how quickly you poll. Startup waits at most five minutes for the initial `--keep-alive` confirmation without consuming the execution allowance. Do not create a separate heartbeat process. Stop an active run before disabling DeLM or revoking its hook trust, since disabled native hooks cannot deliver cancellation.

Give brief progress updates when there is meaningful new information. Distinguish a worker's progress report from a recorded successful check, and describe completion only after the final status is `complete`.

Forward a user's steering message during the run by writing it to a UTF-8 file and calling `update --run-id <uuid> --message-file <absolute-file>`, adding `--inputs-file` for newly selected inputs. If the user asks to stop, call `stop --run-id <uuid>` immediately, then collect the final status and retained partial results.

When status contains a new entry in `questions`, present its actual question text and choices to the user. Track its request ID and present it once, unless the user asks to see it again. Keep monitoring while waiting; unrelated progress does not require repeating an unanswered question. An ordinary update does not answer pending questions. Submit the user's answer with `answer --run-id <uuid> --question-id <request-id> --answers-file <absolute-json>`, where the JSON maps that request's question IDs to arrays of answer strings, for example `{"format":["SVG"]}`. Never invent an answer or reuse it for an unrelated question. Confirm that the answer was applied; it becomes context for both workers.

Use normal native tool permissions if Codex requests access for worker authentication, network, or private project storage. Keep credentials in the existing stock Codex account store.

Report the outcome and link the retained project from the final `path` or `partial_paths`. Include launch instructions from the retained project and describe the checks actually performed. Completed runs include `result.review`, `result.completion`, and recorded command outcomes. For a stopped or failed run, lead with its reason and identify any returned projects as partial work. If no project path is returned, explain that the handoff did not complete and preserve the reported recovery limitation; do not present a guessed worker directory as a finalized result. Preview servers stop with the run; do not link a dead server as a working preview. The original project is unchanged; apply or continue from a retained result only when requested. Do not claim to open a new Codex UI or switch the current thread automatically.
