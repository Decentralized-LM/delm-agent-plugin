import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import test from 'node:test';

const load = async relative => import('data:text/javascript;base64,' + Buffer.from(await readFile(new URL(relative, import.meta.url), 'utf8')).toString('base64'));
const {renderBoard, renderCompact, normalizeBoard, boardSummary, cellWidth, fitText, cleanText} = await load('../hooks/board-render.js');
const {working, preparing, complete, stopped} = await load('../../../tests/fixtures/claude-board/states.js');

function fixture(state = {snapshot: working}, props = {}) {
  const events = [];
  const primitives = Object.fromEntries(['Box', 'Text', 'Button'].map(type => [type, props => ({type, props})]));
  const host = {ui: {resolve: () => primitives}};
  const event = {props: {bodyColumns: 52, maxRows: 2, scroll: {bodyRows: 34}, ...props}};
  const actions = Object.fromEntries(['select', 'details', 'back', 'hide', 'show', 'page', 'refresh'].map(name => [name, (...args) => events.push([name, ...args])]));
  return {host, primitives, event, state, actions, events, render: () => renderBoard(primitives, event, state, actions)};
}
function nodes(tree) {
  if (!tree) return [];
  return [tree, ...[].concat(tree.props?.children || []).flatMap(node => typeof node === 'object' ? nodes(node) : [])];
}
function text(tree) { return nodes(tree).filter(node => node.type !== 'Box').map(node => node.props.label ?? node.props.children).join('\n'); }
function press(tree, key) { const button = nodes(tree).find(node => node.type === 'Button' && node.props.key === key); assert.ok(button, key); button.props.onPress(); }

test('overview uses confirmed ownership and contribution imports without raw internals', () => {
  const f = fixture(); const tree = f.render(), output = text(tree);
  assert.match(output, /Agent 1 · Working/); assert.match(output, /#3 Import endpoint/);
  assert.match(output, /Claimed · Agent 2/); assert.match(output, /Available/);
  assert.match(output, /CSV result shape/); assert.match(output, /Imported by Agent 2/);
  assert.doesNotMatch(output, /local-preview|schema.ts|observed_at|sequence/);
  press(tree, 'task-3'); press(tree, 'shared-publication-1'); press(tree, 'board-hide');
  assert.deepEqual(f.events, [['select', 'task', '3'], ['select', 'contribution', 'publication-1'], ['hide']]);
});

test('preparation and unknown agent observations do not invent work', () => {
  assert.match(text(fixture({snapshot: preparing}).render()), /Preparing/);
  const unknown = structuredClone(working); unknown.agents[0].native_state = 'unknown';
  const output = text(fixture({snapshot: unknown}).render());
  assert.match(output, /Agent 1 · Claimed/); assert.doesNotMatch(output, /Agent 1 · Working/);
  assert.match(text(fixture({snapshot: preparing, phase: 'starting'}).render()), /Starting agents/);
});

test('human collection counts are explicit and selected details stay actionable', () => {
  const snapshot = structuredClone(working); snapshot.tasks.total = 37;
  const f = fixture({snapshot}); let tree = f.render();
  assert.match(text(tree), /Showing 4 of 37/); press(tree, 'view-all-tasks');
  assert.deepEqual(f.events, [['select', 'tasks', null]]);
  f.state.screen = {kind: 'tasks'};
  f.state.pages = {tasks: {items: [{id: 29, title: 'A later task', state: 'available'}], total: 37, next_offset: 30}};
  tree = f.render(); assert.match(text(tree), /A later task/); press(tree, 'next-tasks');
  assert.deepEqual(f.events.at(-1), ['page', 'tasks', 30]);
  f.state.screen = {kind: 'task', id: 29, item: f.state.pages.tasks.items[0]};
  assert.match(text(f.render()), /#29 A later task/);
});

test('delivery, required verification, recovery and cleanup remain separate facts', () => {
  const output = text(fixture({snapshot: complete, screen: {kind: 'details'}}).render());
  assert.match(output, /Local verification required/); assert.match(output, /Changes applied to your project/);
  assert.match(output, /Temporary workspaces removed/); assert.doesNotMatch(output, /verified successfully/);
  const cancelled = text(fixture({snapshot: stopped, screen: {kind: 'details'}}).render());
  assert.match(cancelled, /Stopped · changes saved/); assert.match(cancelled, /not automatically applied/);
  assert.doesNotMatch(cancelled, /Resume|Changes applied to your project/);
  const conflict = text(fixture({snapshot: {...complete, outcome: {conflicts: ['src/app.js'], retained_workspace_path: '/retained'}}, screen: {kind: 'details'}}).render());
  assert.match(conflict, /Delivery needs attention/); assert.match(conflict, /retained/); assert.doesNotMatch(conflict, /workspaces removed/);
});

test('recorded checks distinguish pass and reuse validity from task completion', () => {
  const f = fixture({snapshot: working, screen: {kind: 'task', id: '1'}});
  assert.match(text(f.render()), /Marked done by the agent/);
  f.state.screen = {kind: 'checks'};
  assert.match(text(f.render()), /CSV validation cases\nRecorded pass/);
  f.state.pages = {checks: {items: [{id: 1, summary: 'Changed inputs', passed: true, reusable: false}], total: 1}};
  assert.match(text(f.render()), /Recorded pass · Not valid for reuse/);
});

test('compact presentation yields to native questions, pane visibility and constrained height', () => {
  const f = fixture();
  assert.equal(renderCompact(f.primitives, {...f.event, props: {...f.event.props, hasSurvey: true}}, f.state, f.actions), null);
  assert.equal(renderCompact(f.primitives, f.event, {...f.state, paneShown: true}, f.actions), null);
  assert.equal(renderCompact(f.primitives, {...f.event, props: {...f.event.props, maxRows: 0}}, f.state, f.actions), null);
  const compact = renderCompact(f.primitives, f.event, f.state, f.actions);
  assert.equal(nodes(compact).filter(node => node.type !== 'Box').length, 2);
  press(compact, 'board-show'); assert.deepEqual(f.events, [['show']]);
  const one = renderCompact(f.primitives, {...f.event, props: {...f.event.props, maxRows: 1}}, f.state, f.actions);
  assert.equal(nodes(one).length, 1);
});

test('native update receipt is separate from accepted request revision', () => {
  const f = fixture({snapshot: {...working, revision: 3}, nativeAgents: [{slot: 1, status: 'waiting', deliveredRevision: 2}], screen: {kind: 'details'}});
  const output = text(f.render());
  assert.match(output, /Accepted request revision 3/); assert.match(output, /Agent 1 received revision 2/);
  assert.doesNotMatch(output, /Agent 2 received/);
});

test('terminal controls and direction overrides are removed, Unicode truncation uses cells', () => {
  assert.equal(cleanText('\x1b[31mhello\x1b[0m\x07\u202esecret'), 'hellosecret');
  assert.equal(cleanText('\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\'), 'link');
  assert.equal(cellWidth('界e\u0301👩‍💻'), 5);
  assert.equal(fitText('界界界', 5), '界界…');
  assert.equal(fitText('👩‍💻AB', 3), '👩‍💻…');
  const snapshot = structuredClone(working); snapshot.tasks.items[0].title = '界'.repeat(90);
  for (const width of [16, 24, 52, 88]) {
    const tree = fixture({snapshot}, {bodyColumns: width}).render();
    for (const node of nodes(tree).filter(node => node.type === 'Button')) assert.ok(cellWidth(node.props.label) <= width);
  }
});

test('rendering is deterministic, pure and supplies stable native button keys', () => {
  const f = fixture(); const before = structuredClone(f.state);
  const first = f.render(), second = f.render();
  assert.equal(text(first), text(second)); assert.deepEqual(f.state, before); assert.equal(f.events.length, 0);
  const keys = nodes(first).filter(node => node.type === 'Button').map(node => node.props.key);
  assert.equal(new Set(keys).size, keys.length);
  assert.match(boardSummary({snapshot: working, disconnected: true}), /Updates disconnected/);
  assert.equal(normalizeBoard({snapshot: working}).agents.length, 2);
});

test('small native panes expose agents, claims, sharing and controls together', () => {
  const f = fixture({snapshot: working}, {bodyColumns: 86, scroll: {bodyRows: 9}});
  const output = text(f.render());
  assert.match(output, /Agent 1 · Working · #3 Import endpoint/);
  assert.match(output, /Task queue · 2 of 4 · View all/);
  assert.match(output, /#2 Mapping preview · Claimed · Agent 2/);
  assert.match(output, /Shared context/); assert.match(output, /Imported by Agent 2/);
  assert.match(output, /Details/); assert.match(output, /Hide board/);
});

test('six-row native panes keep both summaries and controls in the visible viewport', () => {
  for (const rows of [4, 5, 6, 7]) {
    const f = fixture({snapshot: working}, {bodyColumns: 76, scroll: {bodyRows: rows}});
    const tree = f.render(), content = tree.props.children;
    assert.ok(content.length <= rows, `${content.length} rows exceed ${rows}`);
    const output = text(tree);
    assert.match(output, /Task queue · 4 tasks · 2 claimed/);
    assert.match(output, /Shared context · 1 shared/);
    assert.match(output, /Details/); assert.match(output, /Hide board/);
    if (rows >= 6) assert.match(output, /Agent 1 · Working · #3 Import endpoint/);
    else assert.match(output, /agents working|Agent 1 · Working/);
    for (const key of ['view-all-tasks', 'view-all-shared', 'board-details', 'board-hide']) press(tree, key);
    assert.deepEqual(f.events, [['select', 'tasks', null], ['select', 'shared', null], ['details'], ['hide']]);
  }
  const f = fixture({snapshot: working, disconnected: true, attention: 'x'.repeat(200)}, {bodyColumns: 76, scroll: {bodyRows: 6}});
  assert.ok(f.render().props.children.length <= 6);
  assert.match(text(f.render()), /Updates disconnected/);
});

test('a running native process without a turn is ready, not invented activity', () => {
  const state = {snapshot: working, nativeAgents: [{slot: 1, id: 'native-a', status: 'running', turn: null}], selectedAgentId: 'native-a'};
  const output = text(fixture(state).render());
  assert.match(output, /Agent 1 · Ready/); assert.match(output, /Viewing Agent 1/);
  assert.doesNotMatch(output, /Agent 1 · Working/);
  state.nativeAgents[0].turn = 'observed-turn';
  assert.match(text(fixture(state).render()), /Agent 1 · Working/);
});

test('detail pagination preserves loading feedback and offers previous and retry controls', () => {
  const f = fixture({snapshot: working, screen: {kind: 'tasks'}, detailLoading: true,
    pages: {tasks: {items: [{id: 9, title: 'Page two', state: 'available'}], total: 16, offset: 8, limit: 8, next_offset: null}}});
  assert.match(text(f.render()), /Loading records/);
  f.state.detailLoading = false; f.state.detailError = 'This page is unavailable. Try again.';
  const tree = f.render(); press(tree, 'previous-tasks'); press(tree, 'retry-tasks');
  assert.deepEqual(f.events, [['page', 'tasks', 0], ['page', 'tasks', 8]]);
  f.state.pages.tasks = {...f.state.pages.tasks, offset: 3, previous_offset: 2};
  press(f.render(), 'previous-tasks'); assert.deepEqual(f.events.at(-1), ['page', 'tasks', 2]);
});

test('authoritative waiting and final states survive stale native running observations', () => {
  const snapshot = structuredClone(working); snapshot.agents[0].native_state = 'waiting';
  const nativeAgents = [{slot: 1, status: 'running', turn: 'old-turn'}];
  assert.match(text(fixture({snapshot, nativeAgents}).render()), /Agent 1 · Waiting/);
  assert.match(text(fixture({snapshot: complete, nativeAgents}).render()), /Agent 1 · Stopped/);
});

test('unavailable sources are not presented as empty collaboration', () => {
  const snapshot = {...preparing, freshness: {unavailable: ['board']}};
  const unavailable = text(fixture({snapshot}).render());
  assert.match(unavailable, /Tasks temporarily unavailable/); assert.doesNotMatch(unavailable, /No tasks shared/);
  delete snapshot.source;
  assert.match(text(fixture({snapshot}).render()), /Loading tasks/);
  assert.match(boardSummary({snapshot: working, disconnected: true}), /Working · Updates disconnected/);
});

test('run detail timestamps identify stale observations without inventing event ages', () => {
  const output = text(fixture({snapshot: {...working, observed_at: 1791144000000}, disconnected: true, screen: {kind: 'details'}}).render());
  assert.match(output, /Last observed 2026-10-04 20:00:00 UTC/);
  assert.match(output, /last observed facts/); assert.doesNotMatch(output, /seconds ago/);
});

test('stale collections disclose provenance and refresh only on user action', () => {
  const f = fixture({snapshot: {...working, revision: 3}, screen: {kind: 'tasks'},
    detailStale: true, detailRevision: 2, pages: {tasks: working.tasks}});
  const tree = f.render(); assert.match(text(tree), /New updates/); assert.match(text(tree), /Showing request revision 2/);
  assert.equal(f.events.length, 0); press(tree, 'detail-refresh'); assert.deepEqual(f.events, [['refresh']]);
  f.state.detailStale = false; f.state.detailRevision = 3;
  assert.doesNotMatch(text(f.render()), /New updates|Showing request revision/);
});

test('selected details keep their captured content until an explicit refresh', () => {
  const oldTask = {...working.tasks.items[0], title: 'Original task title'};
  const f = fixture({snapshot: working, screen: {kind: 'task', id: oldTask.id, item: oldTask}, detailStale: true, detailRevision: 1});
  const output = text(f.render());
  assert.match(output, /Original task title/); assert.doesNotMatch(output, /Mapping preview/);
  assert.match(output, /New updates/);
  f.state.screen.item = working.tasks.items[0]; f.state.detailStale = false;
  assert.match(text(f.render()), /Mapping preview/);
});

test('compact claimed counts cover the full queue rather than just the visible page', () => {
  const snapshot = structuredClone(working); snapshot.tasks.total = 100;
  snapshot.agents[0].task_count = 70; snapshot.agents[1].task_count = 2;
  assert.match(boardSummary({snapshot}), /72 claimed/);
  delete snapshot.agents[0].task_count;
  assert.match(boardSummary({snapshot}), /100 tasks/); assert.doesNotMatch(boardSummary({snapshot}), /2 claimed/);
  snapshot.freshness = {unavailable: ['board']};
  assert.match(boardSummary({snapshot}), /Board updates unavailable/); assert.doesNotMatch(boardSummary({snapshot}), /claimed/);
});

test('an empty page under load or failure is not presented as an empty task board', () => {
  const f = fixture({snapshot: preparing, screen: {kind: 'tasks'}, detailLoading: true});
  assert.match(text(f.render()), /Loading records/); assert.doesNotMatch(text(f.render()), /No records yet/);
  f.state.detailLoading = false; f.state.detailError = 'Page temporarily unavailable.';
  assert.match(text(f.render()), /temporarily unavailable/); assert.doesNotMatch(text(f.render()), /No records yet/);
});

test('the no-Intl fallback preserves emoji modifiers and joined graphemes', async () => {
  const source = await readFile(new URL('../hooks/board-render.js', import.meta.url), 'utf8');
  const fallback = await import('data:text/javascript;base64,' + Buffer.from('const Intl = undefined;\n' + source).toString('base64'));
  assert.equal(fallback.cellWidth('👩🏽‍💻'), 2);
  assert.equal(fallback.fitText('👩🏽‍💻 AB', 3), '👩🏽‍💻…');
  assert.equal(fallback.fitText('🇯🇵🇺🇸abcd', 5), '🇯🇵🇺🇸…');
});
