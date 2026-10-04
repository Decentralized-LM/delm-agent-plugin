export const TOOL_PREFIX = 'mcp__plugin_delm_delm__';

export function parseReply(text) {
  const reply = JSON.parse(text);
  if (!reply || reply.ok !== true) {
    throw new Error(reply?.error || 'DeLM did not return a successful response.');
  }
  return reply.result;
}

export function validateReady(value) {
  if (!value || value.type !== 'ready' || typeof value.run_id !== 'string'
      || typeof value.socket !== 'string' || !value.socket.startsWith('/')
      || typeof value.token !== 'string' || value.token.length < 16
      || typeof value.executable !== 'string' || !value.executable.startsWith('/')
      || !Array.isArray(value.workers) || value.workers.length !== 2
      || value.workers.some((worker, index) => worker?.slot !== index + 1
        || typeof worker.cwd !== 'string' || !worker.cwd.startsWith('/'))
      || value.workers[0].cwd === value.workers[1].cwd
      || !Number.isSafeInteger(value.revision) || value.revision < 1) {
    throw new Error('DeLM returned an invalid native launch contract.');
  }
  return value;
}

export function toolArguments(event) {
  if (Object.hasOwn(event, '_delm')) {
    throw new Error('DeLM coordination credentials must come from the native host.');
  }
  return Object.fromEntries(Object.entries(event)
    .filter(([key]) => !['tool', 'tool_use_id', 'agentId'].includes(key)));
}

export function commandOutcome(result) {
  const native = result?.result;
  const record = native && typeof native === 'object' && !Array.isArray(native) ? native : {};
  const reference = result?.ref;
  return {
    result_ref: typeof reference === 'number' || typeof reference === 'string'
      ? String(reference) : null,
    is_error: Boolean(result?.isError || result?.deny),
    interrupted: record.interrupted === true,
    background_task_id: typeof record.backgroundTaskId === 'string'
      ? record.backgroundTaskId : null,
    timed_out: typeof record.timedOutAfterMs === 'number',
  };
}

export function completeState(status) {
  return ['completed', 'failed', 'killed', 'finished'].includes(status);
}

export function nativeLaunchContext(task, workerPrompt) {
  return [
    'DeLM prepared this explicit request. Start exactly two native fork Agent calls in the same response.',
    'Use subagent_type "fork" and omit model/name/isolation. Both forks inherit this entire conversation, including the worker contract and user request below.',
    'Use this short prompt for each Agent call: "Follow the inherited DeLM worker contract and user request. You are one of two peers. Begin useful work through the shared board now."',
    'Do not copy the contract or user request into the Agent calls; they are already inherited.',
    'Do not inspect the project, plan the implementation, execute shell commands, or do the task in this parent turn.',
    'The native plugin binds each fork to its own prepared workspace. Do not choose a workspace yourself.',
    '',
    'Worker instructions:',
    workerPrompt,
    '',
    'Claude Code host instructions: preserve your inherited setup and use its skills and tools normally.',
    'Use the native DeLM MCP tools for shared coordination. Their native identity is assigned by the host.',
    'Run commands through native Bash. recent_commands contains the observed native IDs for check receipts.',
    'After delm_complete, end your turn promptly. Native permissions remain authoritative.',
    '',
    'User request:',
    task,
  ].join('\n');
}

export function decodeLines(buffer, chunk) {
  const combined = buffer + chunk;
  if (combined.length > 2 * 1024 * 1024) {
    throw new Error('DeLM exceeded its native event size limit.');
  }
  const lines = combined.split('\n');
  const rest = lines.pop();
  return {rest, events: lines.filter(line => line.trim()).map(line => JSON.parse(line))};
}
