// Native presentation only. Rendering never reads files or calls the runtime.
const BLUE = '#6484ed';
const PHASES = {
  preparing: 'Preparing', starting: 'Starting agents', running: 'Working', working: 'Working',
  finishing: 'Finishing', stopping: 'Stopping', completed: 'Complete', complete: 'Complete',
  stopped: 'Stopped', cancelled: 'Stopped', interrupted: 'Needs attention', failed: 'Needs attention',
  error: 'Needs attention', attention: 'Needs attention', unknown: 'Status unavailable',
  prepared: 'Starting agents', awaiting_shutdown: 'Finishing', verifying_shutdown: 'Finishing',
  delivered: 'Changes applied', delivery_conflict: 'Delivery needs attention', recovery_required: 'Needs attention',
  startup_failed: 'Could not start', preparation_failed: 'Could not prepare',
};
const segmenter = typeof Intl !== 'undefined' && typeof Intl.Segmenter === 'function' ? new Intl.Segmenter(undefined, {granularity: 'grapheme'}) : null;

export function cleanText(value, limit = 4000) {
  return String(value ?? '').replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)?/g, '')
    .replace(/\x1b(?:\[[0-?]*[ -/]*[@-~]|[@-_])/g, '')
    .replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g, '')
    .replace(/[\r\t]/g, ' ').slice(0, limit);
}

function graphemes(value) {
  const text = cleanText(value);
  if (segmenter) return Array.from(segmenter.segment(text), part => part.segment);
  const result = [];
  for (const char of text) {
    const previous = result[result.length - 1];
    if (previous && (/^[\p{Mark}\p{Emoji_Modifier}\u200d\ufe0e\ufe0f]$/u.test(char) || previous.endsWith('\u200d')
      || (/^\p{Regional_Indicator}$/u.test(previous) && /^\p{Regional_Indicator}$/u.test(char)))) result[result.length - 1] += char;
    else result.push(char);
  }
  return result;
}

function graphemeCells(value) {
  if (/^[\p{Mark}\u200d\ufe0e\ufe0f]+$/u.test(value)) return 0;
  if (/\p{Extended_Pictographic}|\p{Regional_Indicator}|\u20e3/u.test(value)) return 2;
  const point = value.codePointAt(0);
  return point >= 0x1100 && (point <= 0x115f || point === 0x2329 || point === 0x232a
    || (point >= 0x2e80 && point <= 0xa4cf && point !== 0x303f)
    || (point >= 0xac00 && point <= 0xd7a3) || (point >= 0xf900 && point <= 0xfaff)
    || (point >= 0xfe10 && point <= 0xfe19) || (point >= 0xfe30 && point <= 0xfe6f)
    || (point >= 0xff00 && point <= 0xff60) || (point >= 0xffe0 && point <= 0xffe6)
    || (point >= 0x20000 && point <= 0x3fffd)) ? 2 : 1;
}

export function cellWidth(value) {
  return graphemes(value).reduce((sum, item) => sum + graphemeCells(item), 0);
}

export function fitText(value, width) {
  const text = cleanText(value).replace(/\n+/g, ' ');
  const cells = Math.max(0, Math.floor(width));
  if (!cells) return '';
  if (cellWidth(text) <= cells) return text;
  let result = '', used = 0;
  for (const item of graphemes(text)) {
    const size = graphemeCells(item);
    if (used + size > cells - 1) break;
    result += item; used += size;
  }
  return result + '…';
}

function collection(value) {
  const items = Array.isArray(value) ? value : Array.isArray(value?.items) ? value.items : [];
  return {...value, items, total: Math.max(items.length, Number(value?.total) || 0),
    cursor: value?.cursor ?? value?.next_cursor ?? value?.next_offset ?? null};
}

function agentName(id, agents) {
  if (id === null || id === undefined || id === '') return '';
  const known = agents.find(agent => String(agent.id) === String(id));
  if (known) return known.name;
  const match = String(id).match(/^(?:worker[-_]|agent[-_])?(\d+)$/);
  return match ? `Agent ${match[1]}` : cleanText(id, 120);
}

export function normalizeBoard(input = {}) {
  const snapshot = input.view || input.snapshot || input;
  const agents = (snapshot.agents || []).map((agent, index) => {
    const native = input.nativeAgents?.find(item => (item.slot != null && item.slot === agent.slot)
      || (item.id != null && item.id === agent.native_agent_id));
    const observed = agent.nativeState ?? agent.native_state ?? 'unknown';
    const authoritative = snapshot.finished || ['stopped', 'waiting', 'blocked', 'waiting_for_dependency'].includes(observed);
    return {
    ...agent, id: agent.id ?? agent.worker_id ?? agent.slot ?? String(index + 1),
    name: `Agent ${agent.slot ?? String(agent.id ?? agent.worker_id ?? '').match(/(\d+)$/)?.[1] ?? index + 1}`,
    nativeState: authoritative ? (snapshot.finished ? 'stopped' : observed)
      : native ? (native.status === 'running' ? (native.turn ? 'working' : 'ready') : native.status) : observed,
    taskIds: agent.taskIds ?? agent.task_ids ?? agent.claimed_task_ids ?? [],
    receivedRevision: native?.deliveredRevision ?? agent.receivedRevision ?? agent.received_revision ?? null,
    status: agent.summary ?? agent.status,
  }; });
  const tasks = collection(snapshot.tasks);
  tasks.items = tasks.items.map(task => ({...task, id: String(task.id ?? task.task_id),
    title: cleanText(task.title ?? task.text ?? 'Untitled task'),
    state: task.state ?? task.status ?? 'unknown', owner: task.owner ?? task.owner_id ?? task.claimed_by ?? null}));
  const shared = collection(snapshot.shared ?? snapshot.shared_context);
  shared.items = shared.items.map(entry => ({...entry, id: String(entry.id ?? entry.publication_id),
    kind: entry.kind ?? 'finding', author: entry.author ?? entry.worker ?? entry.worker_id ?? entry.owner,
    title: cleanText(entry.title ?? entry.summary ?? entry.text ?? 'Shared finding', 400),
    text: cleanText(entry.text ?? entry.summary ?? ''), imports: entry.imports ?? entry.imported_by ?? [],
    paths: entry.paths ?? entry.files ?? []}));
  const selectedNative = input.nativeAgents?.find(agent => agent.id === input.selectedAgentId);
  const selectedAgent = agents.find(agent => (input.selectedAgentId && agent.native_agent_id === input.selectedAgentId)
    || (selectedNative && agent.slot === selectedNative.slot));
  return {...snapshot, agents, tasks, shared, checks: collection(snapshot.checks), outcome: snapshot.outcome || {},
    freshness: snapshot.freshness || {}, phase: input.phase || snapshot.phase || snapshot.status || 'preparing',
    runId: snapshot.runId ?? snapshot.run_id, requestRevision: snapshot.requestRevision ?? snapshot.request_revision ?? snapshot.revision,
    attention: snapshot.attention || input.attention, disconnected: input.disconnected || snapshot.freshness?.disconnected,
    selectedAgentName: selectedAgent?.name};
}

export function phaseLabel(view) {
  const outcome = view.outcome || {};
  if (outcome.verificationRequired || outcome.verification_required) return 'Local verification required';
  if (outcome.conflict || outcome.delivery_conflict || outcome.conflicts?.length) return 'Delivery needs attention';
  if (view.phase === 'stopped' && outcome.recovery_saved) return 'Stopped · changes saved';
  return PHASES[view.phase] || 'Status unavailable';
}

function taskNumber(id) { return String(id).startsWith('#') ? String(id) : `#${id}`; }

function taskState(task, view) {
  const owner = agentName(task.owner, view.agents);
  const label = {available: 'Available', open: 'Available', claimed: 'Claimed', done: 'Done', completed: 'Done'}[task.state] || 'State unavailable';
  return owner ? `${label} · ${owner}` : label;
}

function agentState(agent) {
  const native = {running: 'Working', working: 'Working', active: 'Working', starting: 'Starting',
    waiting: 'Waiting', blocked: 'Waiting', ready: 'Ready', waiting_for_dependency: 'Waiting', waiting_for_user: 'Waiting for you',
    completed: 'Turn complete', stopped: 'Stopped', ended: 'Turn complete', failed: 'Needs attention'}[agent.nativeState];
  return native || (agent.taskIds?.length ? 'Claimed' : 'Not started');
}

function compactSummary(view) {
  if (view.disconnected) return `${phaseLabel(view)} · Updates disconnected`;
  const phase = phaseLabel(view);
  if (!['running', 'working'].includes(view.phase)) return phase;
  if (view.freshness.unavailable?.includes('board')) return `${phase} · Board updates unavailable`;
  const active = view.agents.filter(agent => ['running', 'working', 'active'].includes(agent.nativeState)).length;
  if (!active) return `${phase} · ${view.tasks.total} tasks`;
  const completeCounts = view.agents.length && view.agents.every(agent => Number.isSafeInteger(agent.task_count) && agent.task_count >= 0);
  const claimed = completeCounts ? view.agents.reduce((total, agent) => total + agent.task_count, 0)
    : view.tasks.items.filter(task => task.state === 'claimed').length;
  if (!completeCounts && view.tasks.total > view.tasks.items.length) return `${active} ${active === 1 ? 'agent' : 'agents'} working · ${view.tasks.total} tasks`;
  return `${active} ${active === 1 ? 'agent' : 'agents'} working · ${claimed} claimed`;
}

export function boardSummary(state) {
  const view = normalizeBoard(state);
  const lines = [`DeLM · ${compactSummary(view)}`];
  if (view.attention) lines.push(cleanText(view.attention));
  for (const agent of view.agents) lines.push(`${agent.name} · ${agentState(agent)}${agent.taskIds?.length ? ` · ${agent.taskIds.map(taskNumber).join(', ')}` : ''}`);
  if (view.outcome.verificationRequired || view.outcome.verification_required) lines.push('See the final handoff for local verification.');
  return lines.join('\n');
}

function helpers(components, e, actions) {
  const {Box, Text, Button} = components;
  const width = Math.max(1, Math.floor(Number(e.props?.bodyColumns) || 52));
  const padding = width > 24 ? 1 : 0;
  const inner = Math.max(1, width - padding * 2);
  const text = (value, props = {}) => Text({...props, children: cleanText(value)});
  const line = (value, props = {}) => text(fitText(value, inner), props);
  const row = (children, props = {}) => Box({flexDirection: 'row', ...props, children});
  const column = (children, props = {}) => Box({flexDirection: 'column', ...props, children});
  const button = (key, label, callback, props = {}) => Button({key, label: fitText(label, inner), plain: true, onPress: callback || (() => {}), ...props});
  const heading = label => text(label, {bold: true});
  const blank = () => text(' ');
  const controls = back => (inner < 22 ? column : row)([
    button(back ? 'board-back' : 'board-details', back ? 'Back' : 'Details', back ? actions.back : actions.details),
    ...(inner < 22 ? [] : [text('   ')]), button('board-hide', 'Hide board', actions.hide),
  ]);
  return {Box, Text, Button, width, inner, padding, bodyRows: Number(e.props?.scroll?.bodyRows) || 24,
    text, line, row, column, button, heading, blank, controls};
}

function statusLines(h, view) {
  const lines = [h.row([h.text('DeLM', {bold: true, color: BLUE}), h.text(` · ${phaseLabel(view)}`)])];
  if (view.selectedAgentName) lines.push(h.line(`Viewing ${view.selectedAgentName}`, {dimColor: true}));
  if (view.disconnected) lines.push(h.line('Updates disconnected · showing last known state', {dimColor: true}));
  else if (view.freshness.unavailableSources?.length || view.freshness.unavailable_sources?.length || view.freshness.unavailable?.length) lines.push(h.line('Some updates are temporarily unavailable', {dimColor: true}));
  if (view.attention) lines.push(h.text(view.attention));
  return lines;
}

function taskRow(h, task, view, actions) {
  return h.column([
    h.button(`task-${task.id}`, `${taskNumber(task.id)}  ${task.title}`, () => actions.select?.('task', task.id)),
    h.line(`    ${taskState(task, view)}`, {dimColor: true}),
  ], {key: `task-row-${task.id}`});
}

function sharedRow(h, entry, view, actions) {
  const author = agentName(entry.author, view.agents) || 'Agent';
  const imported = entry.imports.map(item => agentName(typeof item === 'object' ? item.worker_id ?? item.agent_id ?? item.id : item, view.agents)).filter(Boolean);
  return h.column([
    h.button(`shared-${entry.id}`, entry.title, () => actions.select?.('contribution', entry.id)),
    h.line(`${author} · ${entry.kind === 'publication' ? 'Code shared' : 'Finding shared'}${imported.length ? ` · Imported by ${imported.join(', ')}` : ''}`, {dimColor: true}),
  ], {key: `shared-row-${entry.id}`});
}

function more(h, type, shown, collection, actions) {
  if (collection.total <= shown && !collection.cursor) return [];
  return [h.row([h.line(`Showing ${shown} of ${collection.total}`, {dimColor: true}), h.text('  '),
    h.button(`view-all-${type}`, 'View all', () => actions.select?.(type, null))])];
}

function emptyCollection(view, kind) {
  const unavailable = view.freshness.unavailable || view.freshness.unavailableSources || view.freshness.unavailable_sources || [];
  if (unavailable.includes('board')) {
    if (!view.source) return kind === 'tasks' ? 'Loading tasks…' : 'Loading shared context…';
    return kind === 'tasks' ? 'Tasks temporarily unavailable' : 'Shared context temporarily unavailable';
  }
  return kind === 'tasks' ? 'No tasks shared yet' : 'No findings or code shared yet';
}

function tinyOverview(h, view, actions) {
  const rows = Math.max(1, Math.floor(h.bodyRows));
  const phase = rows < 5 ? compactSummary(view) : phaseLabel(view);
  const extra = view.disconnected && rows >= 5 ? ' · Updates disconnected' : view.attention ? ' · See details' : '';
  const children = [h.row([h.text('DeLM', {bold: true, color: BLUE}), h.text(fitText(` · ${phase}${extra}`, Math.max(0, h.inner - 4)))])];
  if (rows >= 6 && view.agents.length <= 2) {
    for (const agent of view.agents) {
      const task = view.tasks.items.find(item => agent.taskIds.map(String).includes(String(item.id)));
      children.push(h.line(`${agent.name} · ${agentState(agent)}${task ? ` · ${taskNumber(task.id)} ${task.title}` : ''}`));
    }
    if (!view.agents.length) children.push(h.line('Waiting for agents to start', {dimColor: true}));
  } else if (rows >= 5) children.push(h.line(view.agents.length
    ? view.agents.map(agent => `${agent.name} · ${agentState(agent)}`).join('  ')
    : 'Waiting for agents to start'));
  const unavailable = view.freshness.unavailable?.includes('board');
  const knownCount = view.agents.length && view.agents.every(agent => Number.isSafeInteger(agent.task_count) && agent.task_count >= 0);
  const claimed = knownCount ? view.agents.reduce((total, agent) => total + agent.task_count, 0)
    : view.tasks.total === view.tasks.items.length ? view.tasks.items.filter(task => task.state === 'claimed').length : null;
  const tasks = unavailable ? 'Updates unavailable' : `${view.tasks.total} tasks${claimed != null ? ` · ${claimed} claimed` : ''}`;
  const shared = unavailable ? 'Updates unavailable' : `${view.shared.total} shared`;
  if (rows >= 4) {
    children.push(h.button('view-all-tasks', `Task queue · ${tasks}`, () => actions.select?.('tasks', null)),
      h.button('view-all-shared', `Shared context · ${shared}`, () => actions.select?.('shared', null)));
  } else if (rows >= 3) children.push(h.button('view-all-tasks', `Task queue · ${tasks}`, () => actions.select?.('tasks', null)));
  if (rows >= 2) children.push(h.controls(false));
  else children[0] = h.button('board-details', `DeLM · ${phase} · Details`, actions.details);
  return children;
}

function shortOverview(h, view, actions) {
  const children = statusLines(h, view);
  const spacious = h.bodyRows >= 16;
  if (spacious) children.push(h.blank(), h.heading('Agents'));
  for (const agent of view.agents) {
    const task = view.tasks.items.find(item => agent.taskIds.map(String).includes(String(item.id)));
    children.push(h.line(`${agent.name} · ${agentState(agent)}${task ? ` · ${taskNumber(task.id)} ${task.title}` : ''}`));
  }
  if (!view.agents.length) children.push(h.line('Waiting for agents to start', {dimColor: true}));
  const slots = Math.max(1, Math.min(4, h.bodyRows - children.length - (spacious ? 7 : 4)));
  const ordered = [...view.tasks.items.filter(task => !['done', 'completed'].includes(task.state)),
    ...view.tasks.items.filter(task => ['done', 'completed'].includes(task.state))];
  const tasks = ordered.slice(0, slots);
  const taskHeading = view.tasks.total > tasks.length ? `Task queue · ${tasks.length} of ${view.tasks.total} · View all` : 'Task queue';
  if (spacious) children.push(h.blank());
  children.push(h.button('view-all-tasks', taskHeading, () => actions.select?.('tasks', null)));
  if (!tasks.length) children.push(h.line(emptyCollection(view, 'tasks'), {dimColor: true}));
  for (const task of tasks) {
    const tail = ` · ${taskState(task, view)}`, prefix = `${taskNumber(task.id)} `;
    const title = fitText(task.title, Math.max(1, h.inner - cellWidth(prefix + tail)));
    children.push(h.button(`task-${task.id}`, `${prefix}${title}${tail}`, () => actions.select?.('task', task.id)));
  }
  const shared = view.shared.items[0];
  if (spacious) children.push(h.blank());
  children.push(h.button('view-all-shared', view.shared.total > 1 ? `Shared context · 1 of ${view.shared.total} · View all` : 'Shared context', () => actions.select?.('shared', null)));
  if (!shared) children.push(h.line(emptyCollection(view, 'shared'), {dimColor: true}));
  else {
    const imports = shared.imports.map(item => agentName(typeof item === 'object' ? item.worker_id ?? item.agent_id ?? item.id : item, view.agents)).filter(Boolean);
    const tail = ` · ${agentName(shared.author, view.agents)}${imports.length ? ` · Imported by ${imports.join(', ')}` : ''}`;
    children.push(h.button(`shared-${shared.id}`, `${fitText(shared.title, Math.max(1, h.inner - cellWidth(tail)))}${tail}`,
      () => actions.select?.('contribution', shared.id)));
  }
  if (spacious) children.push(h.blank());
  children.push(h.controls(false));
  return children;
}

function overview(h, view, actions) {
  if (h.bodyRows < 8) return tinyOverview(h, view, actions);
  if (h.bodyRows < 23) return shortOverview(h, view, actions);
  const children = [...statusLines(h, view), h.blank(), h.heading('Agents')];
  if (!view.agents.length) children.push(h.line('Waiting for agents to start', {dimColor: true}));
  for (const agent of view.agents) {
    const current = view.tasks.items.find(task => agent.taskIds.map(String).includes(String(task.id)));
    children.push(h.line(`${agent.name} · ${agentState(agent)}`, {bold: true}));
    if (current) children.push(h.line(`${taskNumber(current.id)} ${current.title}`, {dimColor: true}));
    else if (agent.dependency) children.push(h.line(agent.dependency, {dimColor: true}));
    else if (agent.status && typeof agent.status === 'string') children.push(h.line(`Reported: ${agent.status}`, {dimColor: true}));
    if (current && agent.dependency && ['waiting', 'blocked', 'waiting_for_dependency'].includes(agent.nativeState)) children.push(h.line(agent.dependency, {dimColor: true}));
  }
  children.push(h.blank(), h.heading('Task queue'));
  const tasks = [...view.tasks.items.filter(task => !['done', 'completed'].includes(task.state)),
    ...view.tasks.items.filter(task => ['done', 'completed'].includes(task.state))].slice(0, 4);
  if (!tasks.length) children.push(h.line(emptyCollection(view, 'tasks'), {dimColor: true}));
  for (const task of tasks) children.push(taskRow(h, task, view, actions));
  children.push(...more(h, 'tasks', tasks.length, view.tasks, actions));
  children.push(h.blank(), h.heading('Shared context'));
  const shared = view.shared.items.slice(0, 2);
  if (!shared.length) children.push(h.line(emptyCollection(view, 'shared'), {dimColor: true}));
  for (const entry of shared) children.push(sharedRow(h, entry, view, actions));
  children.push(...more(h, 'shared', shared.length, view.shared, actions), h.blank(), h.controls(false));
  return children;
}

function outcomeLines(h, outcome) {
  const children = [];
  if (outcome.delivered || outcome.deliveryApplied || outcome.delivery_applied) children.push(h.text('Changes applied to your project.'));
  if (outcome.verificationRequired || outcome.verification_required) children.push(h.text('Local verification required. See Claude’s final handoff for the outcome.'));
  if (outcome.cleanupComplete || outcome.cleanup_complete) children.push(h.text('Temporary workspaces removed.'));
  if (outcome.recoveryPath || outcome.recovery_path) children.push(h.text('Unfinished changes saved for recovery. They were not automatically applied to your project.'), h.text(outcome.recoveryPath || outcome.recovery_path, {dimColor: true}));
  if (outcome.retainedPath || outcome.retained_path || outcome.retained_workspace_path) children.push(h.text('Temporary workspaces retained because safe cleanup was not confirmed.'), h.text(outcome.retainedPath || outcome.retained_path || outcome.retained_workspace_path, {dimColor: true}));
  if (outcome.error || outcome.reason) children.push(h.text(outcome.error || outcome.reason));
  if (outcome.conflict || outcome.delivery_conflict || outcome.conflicts?.length) {
    children.push(h.text('Delivery needs attention. See the final handoff before applying changes.'), ...(outcome.conflicts || []).map(path => h.text(path, {dimColor: true})));
    if (outcome.conflicts_total > outcome.conflicts?.length) children.push(h.text(`Showing ${outcome.conflicts.length} of ${outcome.conflicts_total} conflicts.`, {dimColor: true}));
  }
  return children.length ? children : [h.text('No final result yet.', {dimColor: true})];
}

function detail(h, view, screen, actions) {
  const children = [...statusLines(h, view), h.blank()];
  const kind = screen.kind;
  if (view.detailStale) children.push(h.row([h.text('New updates · ', {dimColor: true}), h.button('detail-refresh', 'Refresh', actions.refresh)]));
  if (view.detailRevision != null && view.detailRevision !== view.requestRevision) children.push(h.text(`Showing request revision ${view.detailRevision}`, {dimColor: true}));
  if (view.detailLoading) children.push(h.text('Loading records…', {dimColor: true}));
  if (view.detailError) children.push(h.text(view.detailError));
  if (kind === 'task') {
    const task = (screen.item ? normalizeBoard({tasks: [screen.item]}).tasks.items[0] : null)
      || view.tasks.items.find(item => String(item.id) === String(screen.id));
    children.push(h.heading('Task detail'));
    if (!task) children.push(h.text('This task is not available in the current snapshot.', {dimColor: true}));
    else {
      children.push(h.text(`${taskNumber(task.id)} ${task.title}`, {bold: true}), h.text(taskState(task, view)), h.blank());
      if (task.description || task.body) children.push(h.text(task.description || task.body));
      if (task.dependencies?.length) children.push(h.text(`Depends on ${task.dependencies.map(taskNumber).join(', ')}`));
      if (task.state === 'done' || task.state === 'completed') children.push(h.text('Marked done by the agent. Recorded checks are listed separately.', {dimColor: true}));
      if (task.version != null) children.push(h.text(`Task version ${task.version}`, {dimColor: true}));
    }
  } else if (kind === 'contribution' || (kind === 'shared' && screen.id != null)) {
    const entry = (screen.item ? normalizeBoard({shared: [screen.item]}).shared.items[0] : null)
      || view.shared.items.find(item => String(item.id) === String(screen.id));
    children.push(h.heading('Shared context detail'));
    if (!entry) children.push(h.text('This contribution is not available in the current snapshot.', {dimColor: true}));
    else {
      children.push(h.text(entry.title, {bold: true}), h.text(`${agentName(entry.author, view.agents)} · ${entry.kind === 'publication' ? 'Code shared' : 'Finding shared'}`, {dimColor: true}), h.blank());
      if (entry.text && entry.text !== entry.title) children.push(h.text(entry.text));
      if (entry.paths?.length) children.push(h.blank(), h.heading(entry.file_count > entry.paths.length
        ? `Shared files · ${entry.paths.length} of ${entry.file_count}` : 'Shared files'), ...entry.paths.map(path => h.text(path)));
      if (entry.imports.length) children.push(h.blank(), h.text(`Imported by ${entry.imports.map(item => agentName(typeof item === 'object' ? item.worker_id ?? item.agent_id ?? item.id : item, view.agents)).join(', ')}`));
      else if (entry.kind === 'publication') children.push(h.text('No peer import confirmed.', {dimColor: true}));
    }
  } else if (kind === 'tasks' || kind === 'shared' || kind === 'checks') {
    const records = view[kind];
    children.push(h.heading({tasks: 'All tasks', shared: 'Shared context', checks: 'Recorded checks'}[kind]));
    if (!records.items.length && !view.detailLoading && !view.detailError) children.push(h.text(
      view.freshness.unavailable?.includes('board') ? 'Records temporarily unavailable.' : 'No records yet.', {dimColor: true}));
    for (const record of records.items) {
      if (kind === 'tasks') children.push(taskRow(h, record, view, actions));
      else if (kind === 'shared') children.push(sharedRow(h, record, view, actions));
      else {
        children.push(h.text(record.title || record.name || record.summary || record.scope || 'Recorded check', {bold: true}),
          h.text(`${cleanText(record.outcome || record.status || (record.passed === true ? 'Recorded pass' : record.passed === false ? 'Recorded failure' : 'Outcome unavailable'))}${record.valid_for_reuse === false || record.reusable === false ? ' · Not valid for reuse' : ''}`, {dimColor: true}));
        const author = agentName(record.worker, view.agents);
        if (author || record.revision != null) children.push(h.text([author, record.revision == null ? '' : `Request revision ${record.revision}`].filter(Boolean).join(' · '), {dimColor: true}));
        if (record.reusable == null) children.push(h.text('Current reuse validity not checked by this view.', {dimColor: true}));
      }
    }
    if (view.detailError) children.push(h.button(`retry-${kind}`, 'Try again', () => actions.page?.(kind, records.offset || 0)));
    if (records.cursor) children.push(h.blank(), h.button(`next-${kind}`, 'Next page', () => actions.page?.(kind, records.cursor)));
    const previous = screen.previousCursor ?? records.previous_offset ?? (records.offset > 0 ? Math.max(0, records.offset - (records.limit || records.items.length || 8)) : null);
    if (previous != null) children.push(h.button(`previous-${kind}`, 'Previous page', () => actions.page?.(kind, previous)));
    if (records.total > records.items.length) children.push(h.line(`${records.items.length} shown · ${records.total} total`, {dimColor: true}));
  } else {
    children.push(h.heading('Run details'), h.line(`Run ${view.runId || 'preparing'}`, {dimColor: true}));
    const observedDate = new Date(view.observed_at);
    if (Number.isFinite(view.observed_at) && Number.isFinite(observedDate.getTime())) children.push(h.text(`Last observed ${observedDate.toISOString().replace('T', ' ').replace(/\.\d{3}Z$/, ' UTC')}`, {dimColor: true}));
    if (view.disconnected) children.push(h.text('Updates disconnected. These are the last observed facts.', {dimColor: true}));
    if (view.requestRevision != null) children.push(h.text(`Accepted request revision ${view.requestRevision}`, {dimColor: true}));
    for (const agent of view.agents) if (agent.receivedRevision != null) children.push(h.text(`${agent.name} received revision ${agent.receivedRevision}`, {dimColor: true}));
    children.push(h.blank(), h.button('detail-tasks', `All tasks (${view.tasks.total})`, () => actions.select?.('tasks', null)),
      h.button('detail-shared', `Shared context (${view.shared.total})`, () => actions.select?.('shared', null)),
      h.button('detail-checks', `Recorded checks (${view.checks.total})`, () => actions.select?.('checks', null)),
      h.blank(), h.heading('Result'), ...outcomeLines(h, view.outcome));
  }
  children.push(h.blank(), h.controls(true));
  return children;
}

export function renderBoard(components, e, state = {}, actions = {}) {
  const h = helpers(components, e, actions), view = normalizeBoard(state);
  view.detailLoading = state.detailLoading;
  view.detailError = state.detailError;
  view.detailStale = state.detailStale;
  view.detailRevision = state.detailRevision;
  const screen = state.screen || {kind: 'overview'};
  if (state.pages?.[screen.kind]) {
    const page = state.pages[screen.kind];
    view[screen.kind] = normalizeBoard({[screen.kind]: page})[screen.kind];
  }
  return h.column(screen.kind === 'overview' ? overview(h, view, actions) : detail(h, view, screen, actions),
    {width: h.width, paddingX: h.padding});
}

export function renderCompact(components, e, state = {}, actions = {}) {
  if (e.props?.hasSurvey || Number(e.props?.maxRows) < 1 || state.paneShown || state.suppressCompact) return null;
  const h = helpers(components, e, actions), view = normalizeBoard(state);
  const summary = `DeLM · ${compactSummary(view)}`;
  if (Number(e.props?.maxRows) === 1) {
    return h.button('board-show', fitText(`${summary} · Show board`, h.width), actions.show);
  }
  return h.column([h.text(fitText(summary, h.width)), h.button('board-show', 'Show board', actions.show)], {width: h.width});
}
