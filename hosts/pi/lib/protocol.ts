// Host-neutral helpers shared by the parent extension, the worker extension
// and the tests. Nothing here talks to Pi or to the runtime.

export const TOOL_PREFIX = 'mcp__delm__';
export const MCP_SERVER_NAME = 'delm';

export type Ready = {
  type: 'ready';
  run_id: string;
  session_id: string;
  socket: string;
  token: string;
  executable: string;
  revision: number;
  workers: {slot: number; cwd: string}[];
  [key: string]: unknown;
};

/**
 * Splits the runtime's stdout into JSON records. A line that is not JSON (a
 * stray diagnostic) is returned in `skipped` and never costs the records
 * around it; an unterminated line over the limit is dropped the same way.
 */
export function decodeLines(buffer: string, chunk: string): {rest: string; events: Record<string, unknown>[]; skipped: string[]} {
  const combined = buffer + chunk;
  const lines = combined.split('\n');
  let rest = lines.pop() ?? '';
  const events: Record<string, unknown>[] = [];
  const skipped: string[] = [];
  for (const raw of lines) {
    const line = raw.endsWith('\r') ? raw.slice(0, -1) : raw;
    if (!line.trim()) continue;
    try {
      const value = JSON.parse(line);
      if (value && typeof value === 'object' && !Array.isArray(value)) events.push(value as Record<string, unknown>);
      else skipped.push(line);
    } catch { skipped.push(line.slice(0, 200)); }
  }
  if (rest.length > 2 * 1024 * 1024) { skipped.push(rest.slice(0, 200) + '… (oversized line dropped)'); rest = ''; }
  return {rest, events, skipped};
}

export function validateReady(value: Record<string, unknown>): Ready {
  const workers = value.workers as {slot?: number; cwd?: string}[] | undefined;
  if (!value || value.type !== 'ready' || typeof value.run_id !== 'string'
      || typeof value.socket !== 'string' || !value.socket.startsWith('/')
      || typeof value.token !== 'string' || value.token.length < 16
      || typeof value.executable !== 'string' || !value.executable.startsWith('/')
      || !Array.isArray(workers) || workers.length !== 2
      || workers.some((worker, index) => worker?.slot !== index + 1
        || typeof worker.cwd !== 'string' || !worker.cwd.startsWith('/'))
      || workers[0].cwd === workers[1].cwd
      || !Number.isSafeInteger(value.revision) || (value.revision as number) < 1) {
    throw new Error('DeLM returned an invalid native launch contract.');
  }
  return value as Ready;
}

// Arguments the model supplied, without any transport binding it may have
// tried to forge. The binding is added by the worker extension afterwards.
export function toolArguments(input: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(input).filter(([key]) => key !== '_delm'));
}

export function coordinationTool(toolName: string): string | null {
  return toolName.startsWith(TOOL_PREFIX) ? toolName.slice(TOOL_PREFIX.length) : null;
}

// Pi's bash tool reports the shell's exit status in structuredContent; a
// thrown abort or timeout produces an error result without one. Only an
// observed exit status becomes native process evidence.
export function commandOutcome(event: {result?: unknown; isError?: boolean}): {
  result_ref: string | null; exit_code: number | null; is_error: boolean; interrupted: boolean; timed_out: boolean;
  background_task_id: null;
} {
  const result = (event.result && typeof event.result === 'object' ? event.result : {}) as Record<string, unknown>;
  const structured = (result.structuredContent && typeof result.structuredContent === 'object'
    ? result.structuredContent : {}) as Record<string, unknown>;
  const exit = structured.exit_code;
  const text = Array.isArray(result.content)
    ? (result.content as {type?: string; text?: string}[]).filter(item => item.type === 'text').map(item => item.text || '').join('\n') : '';
  const aborted = /^Command aborted\.?$/m.test(text);
  const timedOut = /^Command timed out after /m.test(text);
  return {
    result_ref: null,
    exit_code: Number.isSafeInteger(exit) ? exit as number : null,
    is_error: Boolean(event.isError),
    interrupted: aborted,
    timed_out: timedOut,
    background_task_id: null,
  };
}

export function turnReason(settled: {aborted?: boolean}, exited: boolean): 'completed' | 'interrupted' | 'failed' {
  if (exited) return 'failed';
  return settled.aborted ? 'interrupted' : 'completed';
}

// The task text a worker receives as its first prompt. The collaboration
// policy itself is appended to the system prompt, not repeated here.
export function launchPrompt(task: string, slot: number, planned = true): string {
  // Peers that plan at the same moment create the same tasks twice, so peer 1 opens the board and
  // peer 2 is prompted once the first task exists (`planned`), or after a bounded wait without one.
  const planning = slot === 1
    ? 'You open the board: share the initial task breakdown promptly (small, independent, claimable tasks), claim one, and start on it.'
    : planned
      ? 'The board already lists your peer\'s initial task breakdown. Read it, claim an available task, and create tasks only for work the board does not already list.'
      : 'Your peer has not shared a task breakdown yet. Read the board first; if it still lists no tasks, share a small breakdown yourself, claim one task, and leave the rest available. Create tasks only for work the board does not already list.';
  return 'DeLM request for peer ' + slot + ' of 2. Follow the DeLM worker policy in your system prompt and begin useful work through the shared board now. '
    + planning + '\n\nUser request:\n' + task;
}

/**
 * Titles compared without case, spacing or separator punctuation, so "calc/ops.py + tests" and
 * "Calc ops.py, tests" collide, while symbols that name different work ("C++" vs "C", "#12") survive.
 */
export function normalizeTitle(title: unknown): string {
  // Tokens are words and numbers, keeping an attached `++`, `#` or `%` (c++, c#, 50%) and issue
  // numbers (#12); everything else, including a `+` or `&` standing between words, is a separator.
  const tokens = String(title ?? '').toLowerCase().match(/#\d+|[\p{L}\p{N}]+(?:\+\+|#(?!\d)|%)?/gu) || [];
  return tokens.join(' ');
}

export type OpenTask = {id: number; task_number?: number; title: string; state: string; owner?: number | null};

/** The open task whose title duplicates `title`, if any; finished tasks never match. */
export function duplicateTask<T extends OpenTask>(title: unknown, tasks: T[]): T | null {
  const wanted = normalizeTitle(title);
  if (!wanted) return null;
  return tasks.find(task => !['done', 'completed'].includes(task.state) && normalizeTitle(task.title) === wanted) || null;
}

export function duplicateTaskReason(task: OpenTask, agentName: (slot: number | null | undefined) => string): string {
  const owner = task.state === 'claimed' && task.owner != null ? 'claimed by ' + agentName(task.owner) : 'available';
  return 'A task with this title already exists: #' + (task.task_number ?? task.id) + ' "' + task.title + '" (' + owner
    + ', task_id ' + task.id + '). Claim it or create a task for different work; do not create it again.';
}

export function hostNotes(): string {
  return [
    'Pi host instructions for DeLM:',
    '- The shared board tools are named ' + TOOL_PREFIX + '<tool>, for example ' + TOOL_PREFIX + 'delm_publish. The policy refers to them by the short name.',
    '- Run checks through the bash tool. Each bash call\'s tool call id (toolu_...) is its native command id. For delm_check_finish and the checks list of delm_complete, copy the full id exactly as recent_commands shows it in board responses; never shorten or guess an id.',
    '- Never supply the _delm field on a board tool; the host binds it.',
    '- After calling delm_complete, finish your reply and end the turn. The runtime resumes you when shared work changes or when the user updates the request.',
    '- Work only inside your current working directory, which is your private copy of the project.',
  ].join('\n');
}

export function updateText(text: string, extra: string[] = []): string {
  const context = extra.filter(Boolean);
  const combined = text + (context.length ? '\n\nAdditional context:\n' + context.join('\n\n') : '');
  let bytes = 0;
  for (const character of combined) {
    const point = character.codePointAt(0)!;
    bytes += point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4;
  }
  if (!combined.trim() || bytes > 128 * 1024) {
    throw new Error('DeLM needs a nonempty update of at most 128 KiB. This update was not sent; shorten it and retry.');
  }
  return combined;
}

export function completeState(status: string): boolean {
  return ['completed', 'failed', 'killed', 'finished'].includes(status);
}

// Provider refusals come back as raw API JSON. Name the cause and the fix in
// one sentence; the raw text stays available in the worker log.
export function explainModelError(raw: string): string {
  let message = raw;
  const json = raw.match(/\{[\s\S]*\}/);
  if (json) {
    try {
      const parsed = JSON.parse(json[0]) as {error?: {message?: string; type?: string}; message?: string};
      message = parsed.error?.message || parsed.message || raw;
    } catch { /* not JSON */ }
  }
  const text = message.trim().replace(/\s+/g, ' ');
  const whole = raw.replace(/\s+/g, ' ');
  if (/out of extra usage/i.test(whole)) {
    return 'Your Anthropic subscription login has no extra usage left, and Pi bills third-party harness use against extra usage. '
      + 'Add extra usage at claude.ai/settings/usage, or run /login in Pi and sign in with an API key instead.';
  }
  if (/\b401\b|authentication|invalid[ -_]?(x-)?api[ -_]?key|unauthorized|token expired|not logged in/i.test(whole)) {
    return 'The provider rejected the login (' + text.slice(0, 160) + '). Run /login in Pi to refresh it.';
  }
  if (/\b429\b|rate[ _]limit|overloaded|\b529\b/i.test(whole)) {
    return 'The provider is rate limiting or overloaded (' + text.slice(0, 160) + '). Wait a minute and run /delm:run again.';
  }
  if (/specified API usage limits?|spend(ing)? limit|regain access on/i.test(whole)) {
    return 'The API key has reached the monthly usage limit set for it (' + text.slice(0, 160) + '). Raise the limit in the Anthropic Console under Settings, Limits, or use another key, then run /delm:run again.';
  }
  if (/credit balance|insufficient_quota|billing_not_active|payment required|\b402\b/i.test(whole)) {
    return 'The provider account has no credit for this login (' + text.slice(0, 160) + '). Add credit or run /login in Pi with another account.';
  }
  if (/model not found|does not exist|unsupported model|not_found_error|^\s*404\b/i.test(whole)) {
    return 'The provider does not offer this session\'s model to the workers (' + text.slice(0, 160) + '). Pick another model with /model and run /delm:run again.';
  }
  return 'The provider returned: ' + text.slice(0, 300);
}
