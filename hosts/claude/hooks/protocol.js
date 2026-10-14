export const TOOL_PREFIX = 'mcp__plugin_delm_delm__';
// Agent counts a Claude Code user can choose. The runtime enforces the same set.
export const AGENT_COUNTS = Object.freeze([2, 4]);
export const DEFAULT_AGENTS = 2;
const COUNT_WORDS = {2: 'two', 4: 'four'};

export function parseReply(text) {
  const reply = JSON.parse(text);
  if (!reply || reply.ok !== true) {
    throw new Error(reply?.error || 'DeLM did not return a successful response.');
  }
  return reply.result;
}

// `expected` is the count this module asked for; a saved contract omits it.
export function validateReady(value, expected = null) {
  if (!value || value.type !== 'ready' || typeof value.run_id !== 'string'
      || typeof value.socket !== 'string' || !value.socket.startsWith('/')
      || typeof value.token !== 'string' || value.token.length < 16
      || typeof value.executable !== 'string' || !value.executable.startsWith('/')
      || !Array.isArray(value.workers) || !AGENT_COUNTS.includes(value.workers.length)
      || (expected !== null && value.workers.length !== expected)
      || value.workers.some((worker, index) => worker?.slot !== index + 1
        || typeof worker.cwd !== 'string' || !worker.cwd.startsWith('/'))
      || new Set(value.workers.map(worker => worker.cwd)).size !== value.workers.length
      || !Number.isSafeInteger(value.revision) || value.revision < 1) {
    throw new Error('DeLM returned an invalid native launch contract.');
  }
  return value;
}

// `/delm:run --agents 4 <task>` chooses the count up front. Only a leading
// option is read, so a task that mentions --agents keeps its own text.
export function parseAgents(args) {
  const text = String(args || '').trim();
  const match = /^--agents(?:=|\s+)(\S+)(?:\s+([\s\S]*))?$/.exec(text)
    || /^--agents=?()$/.exec(text);
  if (!match) return {agents: null, task: text};
  const agents = /^\d+$/.test(match[1]) ? Number(match[1]) : NaN;
  if (!AGENT_COUNTS.includes(agents)) {
    throw new Error('Choose 2 or 4 agents, for example /delm:run --agents 4 <task>.');
  }
  return {agents, task: (match[2] || '').trim()};
}

// The picker's labels. Claude's dialog always adds a free-text row; a typed
// answer that names a supported count ("4", "four", "4 agents") counts too.
export const AGENT_CHOICES = Object.freeze(['2 agents (default)', '4 agents']);
export const AGENT_QUESTION = 'How many agents should DeLM run?';
export const AGENT_QUESTION_RETRY = 'DeLM runs 2 or 4 agents. How many should it run?';
const NUMBER_WORDS = {two: 2, four: 4};

// The chosen count, or null when the answer names no supported count.
export function agentsFromAnswer(answer) {
  const text = String(answer ?? '').trim().toLowerCase();
  const index = AGENT_CHOICES.map(choice => choice.toLowerCase()).indexOf(text);
  if (index >= 0) return AGENT_COUNTS[index];
  const typed = /^(\d+|two|four)(?:\s+agents?)?$/.exec(text);
  const agents = typed ? NUMBER_WORDS[typed[1]] ?? Number(typed[1]) : NaN;
  return AGENT_COUNTS.includes(agents) ? agents : null;
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

export function nativeLaunchContext(task, workerPrompt, agents = DEFAULT_AGENTS) {
  if (!AGENT_COUNTS.includes(agents)) throw new Error('DeLM supports 2 or 4 native peers.');
  const count = COUNT_WORDS[agents];
  return [
    `DeLM prepared this explicit request. Start exactly ${count} native fork Agent calls in the same response.`,
    `Use subagent_type "fork" and omit model/name/isolation. ${agents === 2 ? 'Both' : `All ${count}`} forks inherit this entire conversation, including the worker contract and user request below.`,
    `Use this short prompt for each Agent call: "Follow the inherited DeLM worker contract and user request. You are one of ${count} peers. Begin useful work through the shared board now."`,
    'Do not copy the contract or user request into the Agent calls; they are already inherited.',
    'Do not inspect the project, plan the implementation, execute shell commands, or do the task in this parent turn.',
    'The native plugin binds each fork to its own prepared workspace. Do not choose a workspace yourself.',
    '',
    'Worker instructions:',
    workerPrompt,
    '',
    `Claude Code host instructions: this run has ${count} peers. Preserve your inherited setup and use its skills and tools normally.`,
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

export function followupText(prompt) {
  if (typeof prompt.text !== 'string' || !Array.isArray(prompt.context || [])
      || (prompt.context || []).some(value => typeof value !== 'string')) {
    throw new Error('DeLM could not read the complete text of this update. The peers still have the previous task.');
  }
  if (prompt.attachments?.length) {
    const kinds = [...new Set(prompt.attachments.map(item => item.type))].join(', ');
    throw new Error('Claude cannot forward ' + kinds + ' attachments into existing DeLM peers. '
      + 'This update was not sent; the peers still have the previous task. '
      + 'Use /delm-stop, then start /delm:run with the attachment so every native fork inherits it.');
  }
  // Inline/fenced code and quoted literals are not reference submissions.
  const prose = prompt.text.replace(/```[\s\S]*?```|`[^`\n]*`|(?<!@)"[^"\n]*"|(?<![\p{L}\p{N}_@])'[^'\n]*'(?![\p{L}\p{N}_])/gu, '');
  if (/(^|\s)@(?:"[^"\n]+"|[^\s@]+)/u.test(prose)) {
    throw new Error('Claude expands @references after this forwarding boundary. '
      + 'This update was not sent; the peers still have the previous task. '
      + 'Paste the relevant text, or use /delm-stop and include the reference in a new /delm:run.');
  }
  const context = (prompt.context || []).filter(Boolean);
  const text = prompt.text + (context.length ? '\n\nAdditional context:\n' + context.join('\n\n') : '');
  let bytes = 0;
  for (const character of text) {
    const point = character.codePointAt(0);
    bytes += point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4;
  }
  if (!text.trim() || bytes > 128 * 1024) {
    throw new Error('DeLM needs a nonempty update of at most 128 KiB including additional context. '
      + 'This update was not sent; shorten it and retry.');
  }
  return text;
}
