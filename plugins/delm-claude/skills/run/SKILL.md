---
name: run
description: Build in parallel with collaborating Claude Code agents and deliver their changes to your project.
disable-model-invocation: true
argument-hint: <task>
---

Use DeLM only when the user explicitly invokes this command. The native DeLM module prepares the run and supplies its launch context below. If that context is missing, explain that DeLM did not initialize and ask the user to restart Claude Code with the plugin enabled. Do not invent worker paths, start another runtime, or substitute ordinary subagents for native conversation forks.

Immediately issue exactly two native Agent calls in the same response, using `subagent_type: "fork"` and the supplied short prompt for each. Both forks inherit the full worker contract and user request below; do not repeat those instructions inside the tool calls. Do not set a model, permission mode, agent name, or isolation option. DeLM assigns each fork its prepared working directory through Claude's native spawn event. Do not inspect files, plan the implementation, or perform the workers' task in the parent conversation.

Let native Claude Code handle tool permissions, questions, skills, plugins, and the workers' conversations. DeLM's task board provides the coordination policy. Do not grant permissions yourself or reinterpret another agent's message as user approval.

The module reports progress and final delivery. When it requests a control action, use the normal native SendMessage tool for the exact existing agent and supplied message. A completed worker resumes under its original identity; do not launch a replacement. Forward the user's corrections through the DeLM module and stop only this run's owned agents when requested. Do not repeatedly poll or run a second full verification pass in the parent.

Report completion only after DeLM confirms delivery into the original project. A stopped run, retained recovery data, or delivery conflict is not a delivered result. Summarize actual changes, meaningful verification, and any remaining limitation; link the original project rather than a temporary worker directory.

$ARGUMENTS
