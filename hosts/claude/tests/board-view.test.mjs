import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {setImmediate} from 'node:timers/promises';
import test from 'node:test';

const rendererSource = await readFile(new URL('../hooks/board-render.js', import.meta.url), 'utf8');
const rendererURL = 'data:text/javascript;base64,' + Buffer.from(rendererSource).toString('base64');
const viewSource = await readFile(new URL('../hooks/board-view.js', import.meta.url), 'utf8');
const {registerBoard: createBoardView, observeBoard, validateSnapshot, acceptsSnapshot} = await import('data:text/javascript;base64,'
  + Buffer.from(viewSource.replace("'./board-render.js'", JSON.stringify(rendererURL))).toString('base64'));

const SESSION = 'conversation-1';
const RUN = 'run-1';
const flush = () => setImmediate();
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return {promise, resolve, reject};
};

function runRecord(overrides = {}) {
  return {
    session: SESSION, revision: 1, finished: false, agents: {
      'peer-1': {slot: 1, status: 'running', turn: 'turn-1', deliveredRevision: 1},
    }, ready: {run_id: RUN, session_id: SESSION, token: 'PRIVATE-CONTROL-TOKEN',
      socket: '/private/control.sock', executable: '/retained/old-runtime/delm'}, ...overrides,
  };
}
function snapshot(overrides = {}) {
  return {
    schema_version: 1, type: 'view', session_id: SESSION, run_id: RUN,
    revision: 1, status: 'running', finished: false,
    source: {controller_sequence: 1, board_sequence: 1},
    agents: [{slot: 1, native_agent_id: 'peer-1', native_state: 'working', task_ids: ['1']}],
    tasks: {items: [{id: '1', title: 'Current task', state: 'claimed', owner: 1, version: 1}], total: 1, through_sequence: 1},
    shared: {items: [], total: 0}, checks: {items: [], total: 0}, outcome: {},
    freshness: {observed_at: '2026-10-04T12:00:00Z', unavailable: []}, ...overrides,
  };
}
function channel() {
  let queue = [], waiting = null, ended = false;
  const iterator = {
    returns: 0,
    next() {
      if (queue.length) return Promise.resolve({value: queue.shift(), done: false});
      if (ended) return Promise.resolve({done: true});
      return new Promise(resolve => { waiting = resolve; });
    },
    push(text, stream = 'stdout') {
      const item = {stream, text};
      if (waiting) { const resolve = waiting; waiting = null; resolve({value: item, done: false}); }
      else queue.push(item);
    },
    end() { ended = true; if (waiting) { waiting({done: true}); waiting = null; } },
    return() { this.returns++; this.end(); return Promise.resolve({done: true}); },
    [Symbol.asyncIterator]() { return this; },
  };
  return iterator;
}
function nodes(tree) {
  if (!tree || typeof tree !== 'object') return [];
  return [tree, ...[].concat(tree.children || []).flatMap(nodes)];
}
function text(tree) {
  return nodes(tree).flatMap(node => [typeof node.children === 'string' ? node.children : '', node.label || '']).filter(Boolean).join('\n');
}
function fixture(options = {}) {
  const hooks = [], opens = [], closes = [], commands = [], timers = [], streams = [], forbidden = [], lookups = [], scrolls = [];
  const store = options.store || new Map();
  let session = options.session || SESSION;
  const run = options.run === undefined ? runRecord() : options.run;
  const nativeNode = type => props => ({type, ...props});
  const disallow = name => () => { forbidden.push(name); throw new Error('Forbidden view operation: ' + name); };
  const host = {
    plugin: {root: '/installed/current-plugin'},
    session: {id: async () => session, append: disallow('session.append')},
    store: {get: async key => structuredClone(store.get(key)), set: async (key, value) => { store.set(key, structuredClone(value)); }},
    clock: Object.fromEntries(['after', 'every'].map(kind => [kind, (milliseconds, callback) => {
      const timer = {kind, milliseconds, callback, cancelled: false, cancel() { this.cancelled = true; }};
      timers.push(timer); return timer;
    }])),
    ui: {
      open: spec => { opens.push(spec); return options.open ? options.open(spec) : Promise.resolve({isPlaced: true}); },
      close: async spec => { closes.push(spec); return {}; },
      scroll: async spec => { scrolls.push(spec); return {}; },
      panes: async () => options.panes || [{id: 'delm', isPlaced: true, isShown: true}],
      invalidate: () => {},
      resolve: e => options.resolve ? options.resolve(e) : {Box: nativeNode('Box'), Text: nativeNode('Text'), Button: nativeNode('Button')},
    },
    process: {
      spawn: spec => { commands.push({kind: 'spawn', ...spec}); const stream = channel(); streams.push(stream); return stream; },
      run: async (argv, init) => { commands.push({kind: 'run', argv, ...init});
        return options.read ? options.read(argv, init) : {exitCode: 0, stdout: JSON.stringify(snapshot()), isStdoutTruncated: false}; },
    },
    prompt: {submit: disallow('prompt.submit')}, agent: {spawn: disallow('agent.spawn'), stop: disallow('agent.stop')},
    tool: {call: disallow('tool.call')}, mcp: {call: disallow('mcp.call')},
  };
  if (options.unsupported) delete host.ui.open;
  const view = createBoardView((name, matcher, handler) => {
    hooks.push({name, matcher: typeof matcher === 'function' ? null : matcher,
      handler: typeof matcher === 'function' ? matcher : handler});
  }, options.native ? {} : {
    lookup: async ($, id) => { lookups.push(id); return options.lookup ? options.lookup($, id) : run; },
  });
  async function call(name, event = {}, next = async () => ({}), nativeEvent = name) {
    const hook = hooks.find(item => item.name === name && Object.entries(item.matcher || {}).every(([key, value]) => event[key] === value));
    assert.ok(hook, 'Missing native hook: ' + name);
    const continuation = input => next(input);
    continuation.is = kind => kind === nativeEvent;
    return hook.handler(host, event, continuation);
  }
  function render(component = 'Pane', props = {}) {
    const event = {component, requestId: 'delm', props: {bodyColumns: 72, maxRows: 2, scroll: {bodyRows: 24}, ...props}};
    const hook = hooks.find(item => item.name === 'ui.render' && item.matcher.component === component);
    return hook.handler(host, event, () => ({type: 'NativeFallback'}));
  }
  function press(key, component = 'Pane') {
    const button = nodes(render(component)).find(node => node.key === key);
    assert.ok(button, 'Missing button: ' + key);
    return button.onPress();
  }
  async function emit(value, index = streams.length - 1) {
    streams[index].push(JSON.stringify(value) + '\n'); await flush();
  }
  async function tick(milliseconds, kind = 'every') {
    for (const timer of [...timers]) if (timer.kind === kind && timer.milliseconds === milliseconds && !timer.cancelled && !timer.fired) {
      if (kind === 'after') timer.fired = true;
      timer.callback();
    }
    await flush();
  }
  function stop() { view.end(host, session); for (const stream of streams) stream.end(); }
  return {host, view, render, press, call, emit, tick, stop, select: id => { session = id; },
    streams, commands, opens, closes, forbidden, timers, store, lookups, scrolls};
}

test('begin opens synchronously without taking prompt focus or starting an observer', async () => {
  const pending = deferred(), f = fixture({open: () => pending.promise});
  f.view.begin(f.host, SESSION);
  assert.deepEqual(f.opens, [{id: 'delm', title: 'DeLM', rows: 24, columns: 54}]);
  assert.deepEqual(f.commands, []);
  assert.match(text(f.render()), /Preparing/);
  pending.resolve({isPlaced: true}); await flush(); f.stop();
});

test('status lookup is passive and observer arguments exclude control credentials', async () => {
  const f = fixture();
  await f.view.status(f.host, SESSION); await f.emit(snapshot());
  assert.deepEqual(f.lookups, [SESSION]);
  assert.deepEqual(f.forbidden, []);
  assert.equal(f.commands.length, 1);
  assert.deepEqual(f.commands[0].argv.slice(0, 7), ['/installed/current-plugin/bin/delm', 'claude', 'view', '--run-id', RUN, '--session-id', SESSION]);
  const exposed = JSON.stringify({commands: f.commands, store: [...f.store], rendered: f.render()});
  assert.doesNotMatch(exposed, /PRIVATE-CONTROL-TOKEN|private\/control\.sock|retained\/old-runtime/);
  f.stop();
});

test('status with no current run opens neither pane nor observer', async () => {
  const f = fixture({run: null});
  assert.deepEqual(await f.view.status(f.host, SESSION), {text: 'No DeLM run in this conversation.'});
  assert.deepEqual(f.opens, []); assert.deepEqual(f.commands, []); assert.deepEqual(f.forbidden, []);
});

test('presentation failures leave a passive status fallback and do not invoke control', async () => {
  const f = fixture({open: () => { throw new Error('Pane unavailable'); }, panes: []});
  const result = await f.view.status(f.host, SESSION);
  assert.match(result.text, /DeLM/); assert.match(result.text, /unavailable|disconnected/);
  f.host.ui.resolve = () => { throw new Error('Native renderer unavailable'); };
  assert.deepEqual(f.render(), {type: 'NativeFallback'});
  assert.deepEqual(f.forbidden, []); f.stop();
});

test('unsupported native panes still return a short status without a helper process', async () => {
  const f = fixture({unsupported: true});
  assert.match((await f.view.status(f.host, SESSION)).text, /DeLM/);
  assert.deepEqual(f.commands, []); assert.deepEqual(f.forbidden, []); f.stop();
});

test('snapshot validation bounds collections and preserves independent source clocks', () => {
  const initial = snapshot({revision: 3, source: {controller_sequence: 9, board_sequence: 12}});
  assert.equal(validateSnapshot(initial, SESSION, RUN), initial);
  for (const incoming of [snapshot({revision: 2, source: {controller_sequence: 10, board_sequence: 13}}),
    snapshot({revision: 3, source: {controller_sequence: 8, board_sequence: 13}}),
    snapshot({revision: 3, source: {controller_sequence: 10, board_sequence: 11}})]) assert.equal(acceptsSnapshot(initial, incoming), false);
  assert.equal(acceptsSnapshot(initial, snapshot({revision: 4, source: {controller_sequence: 10, board_sequence: 13}})), true);
  assert.throws(() => validateSnapshot(snapshot({session_id: 'other'}), SESSION, RUN));
  assert.throws(() => validateSnapshot(snapshot({tasks: {items: Array(33).fill({}), total: 33}}), SESSION, RUN));
  assert.throws(() => validateSnapshot(snapshot({tasks: {items: [{}], total: 0}}), SESSION, RUN));
});

test('split stream records update in place and stale snapshots cannot rewind the board', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION);
  const latest = snapshot({revision: 2, source: {controller_sequence: 3, board_sequence: 4},
    tasks: {items: [{id: '2', title: 'Latest task', state: 'available'}], total: 1}});
  const encoded = JSON.stringify(latest);
  f.streams[0].push(encoded.slice(0, 31)); await flush();
  assert.doesNotMatch(text(f.render()), /Latest task/);
  f.streams[0].push(encoded.slice(31) + '\n'); await flush();
  assert.match(text(f.render()), /Latest task/);
  await f.emit(snapshot());
  assert.match(text(f.render()), /Latest task/); assert.doesNotMatch(text(f.render()), /Current task/);
  f.stop();
});

test('oversized and wrong-conversation stream data retain the last valid snapshot', async () => {
  for (const invalid of ['x'.repeat(512 * 1024 + 1), JSON.stringify(snapshot({session_id: 'other'})) + '\n']) {
    const f = fixture(); await f.view.status(f.host, SESSION); await f.emit(snapshot());
    f.streams[0].push(invalid); await flush();
    assert.match(text(f.render()), /Current task/); assert.match(text(f.render()), /Updates disconnected/);
    assert.equal(f.streams[0].returns, 1); assert.deepEqual(f.forbidden, []);
    const retries = f.timers.filter(timer => timer.kind === 'after' && timer.milliseconds >= 1000);
    assert.equal(retries.length, 1); f.stop();
  }
});

test('observer disconnects use bounded retries and leave status available for another attempt', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION);
  for (let attempt = 0; attempt < 4; attempt++) {
    f.streams.at(-1).end(); await flush();
    const retry = f.timers.find(timer => timer.kind === 'after' && timer.milliseconds >= 1000 && !timer.cancelled && !timer.fired);
    if (attempt < 3) {
      assert.ok(retry, 'Transient disconnection should schedule a bounded retry');
      retry.fired = true; retry.callback(); await flush();
    } else assert.equal(retry, undefined, 'Retries must eventually stop');
  }
  assert.equal(f.streams.length, 4);
  await f.view.status(f.host, SESSION);
  assert.equal(f.streams.length, 5, 'The explicit status command may make a fresh passive attempt');
  assert.deepEqual(f.forbidden, []); f.stop();
});

test('hiding survives restoration and status explicitly reopens the same run', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION); await f.emit(snapshot());
  f.press('board-hide'); await flush();
  assert.equal(f.store.get('native-board:' + SESSION).hidden, true);
  assert.deepEqual(f.closes, [{id: 'delm'}]);
  assert.equal(f.streams[0].returns, 1);
  assert.match(f.commands.at(-1).argv.join(' '), /--interval-ms 1000$/);
  const restored = fixture({store: f.store, panes: []}); await restored.view.restore(restored.host, SESSION);
  assert.deepEqual(restored.opens, []);
  assert.equal(restored.streams.length, 1);
  assert.match(restored.commands[0].argv.join(' '), /--interval-ms 1000$/);
  await restored.emit(snapshot({status: 'stopping'}));
  assert.match(text(restored.render('AbovePrompt')), /Stopping/);
  await restored.view.status(restored.host, SESSION); await flush();
  assert.equal(restored.opens.length, 1); assert.equal(restored.store.get('native-board:' + SESSION).hidden, false);
  assert.deepEqual(restored.forbidden, []); f.stop(); restored.stop();
});

test('native user close persists hiding while denied or plugin closure does not', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION); await flush();
  await f.call('ui.close', {id: 'delm', origin: {kind: 'person'}}, async () => ({deny: 'Native refusal'}));
  await flush(); assert.equal(f.store.get('native-board:' + SESSION).hidden, false);
  await f.call('ui.close', {id: 'delm', origin: {kind: 'plugin'} });
  await flush(); assert.equal(f.store.get('native-board:' + SESSION).hidden, false);
  await f.call('ui.close', {id: 'delm', origin: {kind: 'person'} });
  await flush(); assert.equal(f.store.get('native-board:' + SESSION).hidden, true);
  assert.deepEqual(f.forbidden, []); f.stop();
});

test('late observer records and pane-open completions cannot repaint another conversation', async () => {
  const opening = deferred(); let count = 0;
  const f = fixture({open: () => ++count === 2 ? opening.promise : Promise.resolve({isPlaced: true})});
  await f.view.status(f.host, SESSION);
  const reopening = f.view.status(f.host, SESSION); await flush();
  f.streams[0].push(JSON.stringify(snapshot({tasks: {items: [{id: 'old', title: 'OLD CONVERSATION', state: 'available'}], total: 1}})) + '\n');
  f.select('conversation-2'); f.view.begin(f.host, 'conversation-2');
  opening.resolve({isPlaced: true}); await reopening; await flush();
  assert.match(text(f.render()), /Preparing/); assert.doesNotMatch(text(f.render()), /OLD CONVERSATION/);
  assert.ok(f.streams[0].returns >= 1, 'Conversation replacement must close its old observer'); f.stop();
});

test('preparation completing during pane opening starts exactly one observer when ready', async () => {
  const opening = deferred(), f = fixture({open: () => opening.promise});
  f.view.begin(f.host, SESSION);
  f.view.observe(f.host, runRecord());
  assert.equal(f.streams.length, 0, 'Observation waits for the native pane API to be ready');
  opening.resolve({isPlaced: true}); await flush();
  assert.equal(f.streams.length, 1);
  f.view.observe(f.host, runRecord({revision: 2})); await flush();
  assert.equal(f.streams.length, 1, 'Repeated native preparation facts must reuse the same observer');
  await f.emit(snapshot()); assert.match(text(f.render()), /Current task/); f.stop();
});

test('late opening for a replaced conversation cannot start its prepared observer', async () => {
  const opening = deferred(); let count = 0;
  const f = fixture({open: () => ++count === 1 ? opening.promise : Promise.resolve({isPlaced: true})});
  f.view.begin(f.host, SESSION); f.view.observe(f.host, runRecord());
  f.select('conversation-2'); f.view.begin(f.host, 'conversation-2');
  opening.resolve({isPlaced: true}); await flush();
  assert.equal(f.streams.length, 0);
  assert.match(text(f.render()), /Preparing/); f.stop();
});

test('the native command wrapper observes preparation before the launch handler returns', async () => {
  const pending = deferred(), f = fixture({native: true});
  let entered = false, settled = false;
  const launching = f.call('command.*', {command: 'delm:run', args: 'Build the task'}, async () => {
    entered = true;
    assert.equal(f.opens.length, 1, 'The command opens its pane before awaiting preparation');
    return pending.promise;
  }, 'command.run').then(result => { settled = true; return result; });
  await flush(); assert.equal(entered, true); assert.equal(settled, false);
  observeBoard(runRecord()); await f.tick(250);
  assert.equal(settled, false); assert.equal(f.streams.length, 1);
  await f.emit(snapshot()); assert.match(text(f.render()), /Current task/);
  pending.resolve({text: 'Launch accepted'});
  assert.deepEqual(await launching, {text: 'Launch accepted'});
  assert.equal(f.streams.length, 1); assert.deepEqual(f.forbidden, []); f.stop();
});

test('session wildcard hooks preserve native continuations and dispose only their run', async () => {
  const f = fixture({native: true});
  f.store.set('native-run:' + SESSION, runRecord());
  const events = [];
  const started = await f.call('session.*', {}, async () => { events.push('start'); return {native: 'started'}; }, 'session.start');
  assert.deepEqual(started, {native: 'started'}); await flush();
  assert.equal(f.streams.length, 1);
  const ended = await f.call('session.*', {sessionId: SESSION}, async () => { events.push('end'); return {native: 'ended'}; }, 'session.end');
  assert.deepEqual(ended, {native: 'ended'}); assert.deepEqual(events, ['start', 'end']);
  assert.ok(f.streams[0].returns >= 1); assert.deepEqual(f.render(), {type: 'NativeFallback'});
  assert.deepEqual(f.forbidden, []);
});

test('changing conversation clears old data before a slow passive lookup finishes', async () => {
  const pending = deferred();
  const f = fixture({lookup: async ($, id) => id === SESSION ? runRecord() : pending.promise});
  await f.view.status(f.host, SESSION); await f.emit(snapshot());
  f.select('conversation-2'); const restoring = f.view.restore(f.host, 'conversation-2');
  assert.doesNotMatch(text(f.render()), /Current task/);
  pending.resolve(null); await restoring; f.stop();
});

test('the native pump clears a changed conversation without relying on lifecycle events', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION); await f.emit(snapshot());
  f.select('conversation-2'); await f.tick(250);
  assert.deepEqual(f.render(), {type: 'NativeFallback'});
  assert.ok(f.streams[0].returns >= 1);
  assert.ok(f.timers.filter(timer => timer.kind === 'every').every(timer => timer.cancelled));
  assert.deepEqual(f.forbidden, []); f.stop();
});

test('identity checks do not overlap or dispose a newer view after a delayed response', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION); await f.emit(snapshot());
  const identity = deferred(); let queries = 0;
  f.host.session.id = () => { queries++; return identity.promise; };
  await f.tick(250); await f.tick(250);
  assert.equal(queries, 1);
  f.select('conversation-2'); f.view.begin(f.host, 'conversation-2');
  identity.resolve('unrelated-old-identity'); await flush();
  assert.match(text(f.render()), /Preparing/);
  assert.deepEqual(f.forbidden, []); f.stop();
});

test('a failed identity read clears only the view and leaves command handling available', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION); await f.emit(snapshot());
  f.host.session.id = async () => { throw new Error('Native identity temporarily unavailable'); };
  await f.tick(250);
  assert.deepEqual(f.render(), {type: 'NativeFallback'});
  assert.deepEqual(await f.call('command.*', {command: 'help'}, async () => ({text: 'Native help'}), 'command.run'), {text: 'Native help'});
  assert.deepEqual(f.forbidden, []); f.stop();
});

test('ordinary command and session events clear completed foreign boards before continuing', async () => {
  for (const [hook, event, nativeEvent] of [['command.*', {command: 'help'}, 'command.run'], ['session.*', {}, 'session.resume']]) {
    const f = fixture(); await f.view.status(f.host, SESSION);
    await f.emit(snapshot({status: 'delivered', finished: true, outcome: {delivered: true}}));
    f.select('conversation-2');
    let continued = false;
    const result = await f.call(hook, event, async () => {
      continued = true;
      assert.deepEqual(f.render(), {type: 'NativeFallback'}, 'The previous result must be cleared before native continuation');
      return {native: 'continued'};
    }, nativeEvent);
    assert.equal(continued, true); assert.deepEqual(result, {native: 'continued'});
    assert.deepEqual(f.forbidden, []); f.stop();
  }
});

test('a delayed native close for the old pane cannot hide a newly started run', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION);
  const closing = deferred();
  const previousClose = f.call('ui.close', {id: 'delm', origin: {kind: 'person'}}, () => closing.promise);
  f.select('conversation-2'); f.view.begin(f.host, 'conversation-2'); await flush();
  closing.resolve({}); await previousClose; await flush();
  assert.equal(f.store.get('native-board:conversation-2').hidden, false);
  f.stop();
});

test('native transcript views suppress the board for unrelated agents', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION); await f.emit(snapshot());
  assert.deepEqual(f.render('Pane', {view: {agentId: 'unrelated'}}), {type: 'NativeFallback'});
  assert.match(text(f.render('Pane', {view: {agentId: 'peer-1'}})), /Current task/);
  f.stop();
});

test('final snapshots end observation without retries and remain reopenable', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION);
  await f.emit(snapshot({status: 'delivered', finished: true, outcome: {delivered: true, verification_required: true, cleanup_complete: true}}));
  assert.equal(f.streams[0].returns, 1, 'A confirmed final snapshot must end its observer');
  assert.equal(f.store.get('native-board:' + SESSION).finalSnapshot.finished, true);
  assert.equal(f.timers.filter(timer => timer.kind === 'after' && timer.milliseconds >= 1000 && !timer.cancelled).length, 0);
  assert.match(text(f.render()), /Local verification required/);
  f.stop();
});

test('an older final snapshot cannot end observation of a newer request revision', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION);
  await f.emit(snapshot({revision: 3, source: {controller_sequence: 9, board_sequence: 12}}));
  await f.emit(snapshot({revision: 2, status: 'delivered', finished: true,
    source: {controller_sequence: 8, board_sequence: 11}, outcome: {delivered: true}}));
  assert.equal(f.streams[0].returns, 0);
  assert.equal(f.store.get('native-board:' + SESSION).finalSnapshot, null);
  assert.doesNotMatch(text(f.render()), /Changes applied/);
  await f.emit(snapshot({revision: 3, source: {controller_sequence: 10, board_sequence: 13},
    tasks: {items: [{id: '2', title: 'Still observing', state: 'available'}], total: 1}}));
  assert.match(text(f.render()), /Still observing/); f.stop();
});

test('finished restoration loads saved board details after asynchronous pane opening', async () => {
  const opening = deferred(), reading = deferred();
  const f = fixture({run: runRecord({finished: true, final: {status: 'delivered', delivery: {delivered: true}}}),
    open: () => opening.promise, read: () => reading.promise});
  await f.view.restore(f.host, SESSION);
  assert.equal(f.commands.length, 0);
  opening.resolve({isPlaced: true}); await flush();
  assert.equal(f.commands.filter(command => command.kind === 'run').length, 1);
  const status = f.view.status(f.host, SESSION); await flush();
  assert.equal(f.commands.filter(command => command.kind === 'run').length, 1, 'Concurrent final reads coalesce');
  reading.resolve({exitCode: 0, isStdoutTruncated: false, stdout: JSON.stringify(snapshot({
    status: 'delivered', finished: true, outcome: {delivered: true, cleanup_complete: true, verification_required: true},
  }))});
  await status; await flush();
  assert.match(text(f.render()), /Current task/); assert.match(text(f.render()), /Local verification required/);
  f.press('board-details'); assert.match(text(f.render()), /Temporary workspaces removed/);
  assert.equal(f.streams.length, 0); f.stop();
});

test('a delayed final read cannot repaint a replacement conversation', async () => {
  const reading = deferred();
  const f = fixture({run: runRecord({finished: true, final: {status: 'delivered', delivery: {delivered: true}}}), read: () => reading.promise});
  const restoring = f.view.restore(f.host, SESSION); await flush();
  assert.equal(f.commands.filter(command => command.kind === 'run').length, 1);
  f.select('conversation-2'); f.view.begin(f.host, 'conversation-2');
  reading.resolve({exitCode: 0, isStdoutTruncated: false, stdout: JSON.stringify(snapshot({status: 'delivered', finished: true}))});
  await restoring; await flush();
  assert.match(text(f.render()), /Preparing/); assert.doesNotMatch(text(f.render()), /Current task/);
  assert.equal(f.streams.length, 0); f.stop();
});

test('detail paging reaches records beyond 24 using the stable collection anchor', async () => {
  const f = fixture({read: async argv => {
    const offset = Number(argv[argv.indexOf('--offset') + 1]);
    return {exitCode: 0, isStdoutTruncated: false, stdout: JSON.stringify(snapshot({
      tasks: {items: Array.from({length: Math.min(8, 40 - offset)}, (_, index) => ({id: String(offset + index + 1), title: 'Task ' + (offset + index + 1), state: 'available'})), total: 40, through_sequence: 99, next_offset: offset + 8 < 40 ? offset + 8 : null},
    }))};
  }});
  await f.view.status(f.host, SESSION); await f.emit(snapshot({tasks: {
    items: Array.from({length: 8}, (_, index) => ({id: String(index + 1), title: 'Task ' + (index + 1), state: 'available'})), total: 40, through_sequence: 99, next_offset: 8,
  }}));
  assert.match(text(f.render()), /Showing 4 of 40/);
  f.press('view-all-tasks'); await flush();
  for (let page = 0; page < 3; page++) { f.press('next-tasks'); await flush(); }
  assert.match(text(f.render()), /Task 25/);
  const reads = f.commands.filter(command => command.kind === 'run');
  assert.equal(reads.length, 4);
  for (const [index, read] of reads.entries()) {
    if (index === 0) assert.ok(!read.argv.includes('--through-sequence'));
    else assert.equal(read.argv[read.argv.indexOf('--through-sequence') + 1], '99');
    assert.equal(read.argv[read.argv.indexOf('--limit') + 1], '8');
    assert.equal(read.stdin, '');
  }
  f.press('task-25'); assert.match(text(f.render()), /Task detail/); assert.match(text(f.render()), /Task 25/);
  f.press('board-back'); assert.match(text(f.render()), /All tasks/); assert.match(text(f.render()), /Task 25/);
  assert.deepEqual(f.forbidden, []); f.stop();
});

test('a late detail response cannot replace the new conversation view', async () => {
  const pending = deferred();
  const f = fixture({read: () => pending.promise});
  await f.view.status(f.host, SESSION); await f.emit(snapshot({tasks: {
    items: [{id: '1', title: 'Current task', state: 'available'}], total: 40, through_sequence: 99, next_offset: 8,
  }}));
  f.press('view-all-tasks'); await flush();
  assert.equal(f.commands.filter(command => command.kind === 'run').length, 1);
  f.select('conversation-2'); f.view.begin(f.host, 'conversation-2');
  pending.resolve({exitCode: 0, isStdoutTruncated: false, stdout: JSON.stringify(snapshot({tasks: {
    items: [{id: '25', title: 'OLD PRIVATE TASK', state: 'available'}], total: 40, through_sequence: 99,
  }}))});
  await flush();
  assert.match(text(f.render()), /Preparing/); assert.doesNotMatch(text(f.render()), /OLD PRIVATE TASK/);
  f.stop();
});

function pageSnapshot(title, {revision = 1, boardSequence = 1, owner = 1, offset = 0, count = 8, next = offset + count} = {}) {
  return snapshot({revision, source: {controller_sequence: revision, board_sequence: boardSequence}, tasks: {
    items: Array.from({length: count}, (_, index) => ({id: String(offset + index + 1), title: index === 0 ? title : `Task ${offset + index + 1}`, state: 'claimed', owner})),
    total: 40, through_sequence: 99, offset, limit: 8, next_offset: next,
  }});
}
const viewReply = value => ({exitCode: 0, isStdoutTruncated: false, stdout: JSON.stringify(value)});

test('a page overtaken by a newer live revision is rejected and retried once', async () => {
  const first = deferred(), retry = deferred(); let reads = 0;
  const f = fixture({read: () => ++reads === 1 ? first.promise : retry.promise});
  await f.view.status(f.host, SESSION); await f.emit(pageSnapshot('Original task'));
  f.press('view-all-tasks'); await flush();
  await f.emit(pageSnapshot('Current live task', {revision: 2, boardSequence: 3}));
  first.resolve(viewReply(pageSnapshot('STALE PAGE'))); await flush();
  assert.equal(reads, 2); assert.doesNotMatch(text(f.render()), /STALE PAGE/);
  retry.resolve(viewReply(pageSnapshot('Fresh page', {revision: 2, boardSequence: 3, owner: 2})));
  await flush();
  assert.match(text(f.render()), /Fresh page/); assert.match(text(f.render()), /Claimed · Agent 2/);
  const requests = f.commands.filter(command => command.kind === 'run');
  assert.deepEqual(requests[0].argv, requests[1].argv, 'Retry retains the requested page and collection anchor');
  assert.doesNotMatch(text(f.render()), /Showing request revision 1/);
  f.stop();
});

test('a repeatedly stale page stops after one retry and leaves an explicit retry control', async () => {
  let reads = 0;
  const f = fixture({read: async () => { reads++; return viewReply(pageSnapshot('STALE PAGE')); }});
  await f.view.status(f.host, SESSION); await f.emit(pageSnapshot('Current live task', {revision: 2, boardSequence: 3}));
  f.press('view-all-tasks'); await flush();
  assert.equal(reads, 2);
  assert.doesNotMatch(text(f.render()), /STALE PAGE/);
  assert.match(text(f.render()), /This page is unavailable/);
  assert.ok(nodes(f.render()).some(node => node.key === 'retry-tasks'));
  f.stop();
});

test('cached pages and selected details expose newer sources without silently replacing what is being read', async () => {
  let response = pageSnapshot('Captured task');
  const f = fixture({read: async argv => {
    if (!argv.includes('--item-id')) return viewReply(response);
    const id = argv[argv.indexOf('--item-id') + 1];
    return viewReply({...response, tasks: {...response.tasks,
      items: response.tasks.items.filter(item => String(item.id) === id), item_id: Number(id), limit: 1, next_offset: null}});
  }});
  await f.view.status(f.host, SESSION); await f.emit(response);
  f.press('view-all-tasks'); await flush();
  response = pageSnapshot('Updated task', {boardSequence: 2, owner: 2});
  await f.emit(response);
  assert.match(text(f.render()), /Captured task/); assert.doesNotMatch(text(f.render()), /Updated task/);
  assert.match(text(f.render()), /New updates/);
  f.press('detail-refresh'); await flush();
  assert.match(text(f.render()), /Updated task/); assert.doesNotMatch(text(f.render()), /New updates/);
  f.press('task-1');
  response = pageSnapshot('Newest task', {revision: 2, boardSequence: 3, owner: 1});
  await f.emit(response);
  assert.match(text(f.render()), /Updated task/); assert.doesNotMatch(text(f.render()), /Newest task/);
  assert.match(text(f.render()), /Showing request revision 1/); assert.match(text(f.render()), /New updates/);
  f.press('detail-refresh'); await flush();
  assert.match(text(f.render()), /Newest task/); assert.match(text(f.render()), /Claimed · Agent 1/);
  assert.doesNotMatch(text(f.render()), /Showing request revision 1|New updates/);
  for (const read of f.commands.filter(command => command.kind === 'run')) {
    if (read.argv.includes('--item-id')) {
      assert.equal(read.argv[read.argv.indexOf('--item-id') + 1], '1');
      assert.ok(!read.argv.some(arg => ['--through-sequence', '--offset', '--limit'].includes(arg)));
    } else assert.ok(!read.argv.includes('--through-sequence'), 'Explicit collection refresh uses the latest anchor');
  }
  f.press('board-back');
  assert.match(text(f.render()), /Updated task/); assert.doesNotMatch(text(f.render()), /Newest task/);
  assert.match(text(f.render()), /New updates/);
  assert.deepEqual(f.forbidden, []); f.stop();
});

test('Previous page returns to the exact prior offset when page lengths vary', async () => {
  const sizes = new Map([[0, 5], [5, 3], [8, 7]]), offsets = [];
  const f = fixture({read: async argv => {
    const offset = Number(argv[argv.indexOf('--offset') + 1]); offsets.push(offset);
    return viewReply(pageSnapshot(`Page at ${offset}`, {offset, count: sizes.get(offset)}));
  }});
  await f.view.status(f.host, SESSION); await f.emit(pageSnapshot('Initial page', {count: 5}));
  f.press('view-all-tasks'); await flush();
  f.press('next-tasks'); await flush();
  f.press('next-tasks'); await flush();
  assert.match(text(f.render()), /Page at 8/);
  f.press('previous-tasks'); await flush();
  assert.match(text(f.render()), /Page at 5/);
  f.press('previous-tasks'); await flush();
  assert.match(text(f.render()), /Page at 0/);
  assert.deepEqual(offsets, [0, 5, 8, 5, 0]);
  assert.ok(!nodes(f.render()).some(node => node.key === 'previous-tasks'));
  f.stop();
});

test('detail navigation scrolls to the start and Back restores the semantic source key', async () => {
  const f = fixture(); await f.view.status(f.host, SESSION); await f.emit(snapshot());
  f.press('task-1'); await f.tick(0, 'after');
  assert.deepEqual(f.scrolls.at(-1), {to: 'start', in: 'delm', block: 'nearest'});
  f.press('board-back'); await f.tick(0, 'after');
  assert.deepEqual(f.scrolls.at(-1), {to: {key: 'task-1'}, in: 'delm', block: 'nearest'});
  f.press('board-details'); await f.tick(0, 'after');
  f.press('detail-tasks'); await f.tick(0, 'after');
  assert.deepEqual(f.scrolls.at(-1), {to: 'start', in: 'delm', block: 'nearest'});
  f.press('board-back'); await f.tick(0, 'after');
  assert.deepEqual(f.scrolls.at(-1), {to: {key: 'detail-tasks'}, in: 'delm', block: 'nearest'});
  f.press('board-back'); await f.tick(0, 'after');
  assert.deepEqual(f.scrolls.at(-1), {to: {key: 'board-details'}, in: 'delm', block: 'nearest'});
  assert.deepEqual(f.forbidden, []);
  f.press('task-1'); const beforeDispose = f.scrolls.length;
  f.stop(); await f.tick(0, 'after');
  assert.equal(f.scrolls.length, beforeDispose, 'A disposed view must not scroll a later conversation');
});

test('refreshing a later collection page preserves its current offset', async () => {
  let boardSequence = 1;
  const f = fixture({read: async argv => {
    const offset = Number(argv[argv.indexOf('--offset') + 1]);
    return viewReply(pageSnapshot(`Page ${offset}`, {offset, boardSequence}));
  }});
  await f.view.status(f.host, SESSION); await f.emit(pageSnapshot('Overview'));
  f.press('view-all-tasks'); await flush();
  f.press('next-tasks'); await flush();
  boardSequence = 2; await f.emit(pageSnapshot('New overview', {boardSequence}));
  f.press('detail-refresh'); await flush();
  const last = f.commands.filter(command => command.kind === 'run').at(-1);
  assert.equal(last.argv[last.argv.indexOf('--offset') + 1], '8');
  assert.match(text(f.render()), /Page 8/); assert.doesNotMatch(text(f.render()), /New updates/);
  f.stop();
});

test('selecting an overview row captures the visible snapshot instead of a cached detail page', async () => {
  const f = fixture({read: async () => viewReply(pageSnapshot('Old cached version'))});
  await f.view.status(f.host, SESSION); await f.emit(pageSnapshot('Original overview'));
  f.press('view-all-tasks'); await flush(); f.press('board-back');
  await f.emit(pageSnapshot('Visible task', {revision: 2, boardSequence: 2}));
  assert.match(text(f.render()), /Visible task/);
  f.press('task-1');
  assert.match(text(f.render()), /Visible task/);
  assert.doesNotMatch(text(f.render()), /Old cached version|New updates|Showing request revision 1/);
  f.stop();
});

test('hiding during preparation persists that choice when the real run identity arrives', async () => {
  const f = fixture(); f.view.begin(f.host, SESSION); await flush();
  f.press('board-hide'); await flush();
  assert.equal(f.store.get('native-board:' + SESSION).hidden, true);
  f.view.observe(f.host, runRecord()); await flush();
  assert.equal(f.store.get('native-board:' + SESSION).runId, RUN);
  assert.equal(f.store.get('native-board:' + SESSION).hidden, true);
  f.stop();
});

test('refreshing an unavailable exact item preserves the captured detail and explains the limit', async () => {
  const f = fixture({read: async () => viewReply(snapshot({source: {controller_sequence: 1, board_sequence: 2},
    tasks: {items: [], total: 50, item_id: 45, limit: 1, next_offset: null}}))});
  await f.view.status(f.host, SESSION);
  await f.emit(snapshot({tasks: {items: [{id: '45', title: 'Captured high-ID task', state: 'claimed', owner: 1}], total: 50, through_sequence: 99, offset: 0}}));
  f.press('task-45');
  await f.emit(pageSnapshot('Other overview task', {boardSequence: 2}));
  f.press('detail-refresh'); await flush();
  assert.match(text(f.render()), /Captured high-ID task/);
  assert.match(text(f.render()), /previous details remain/i);
  assert.match(text(f.render()), /New updates/);
  f.stop();
});

test('an overview-selected item beyond the first page refreshes by exact identity', async () => {
  const f = fixture({read: async argv => {
    assert.deepEqual(argv.slice(7), ['--collection', 'tasks', '--item-id', '45']);
    return viewReply(snapshot({source: {controller_sequence: 1, board_sequence: 2}, tasks: {
      items: [{id: '45', title: 'Updated high-ID task', state: 'done', owner: 2}], total: 50,
      item_id: 45, limit: 1, next_offset: null,
    }}));
  }});
  await f.view.status(f.host, SESSION);
  await f.emit(snapshot({tasks: {items: [{id: '45', title: 'Original high-ID task', state: 'claimed', owner: 1}], total: 50, through_sequence: 99, offset: 0}}));
  f.press('task-45');
  await f.emit(pageSnapshot('Other overview task', {boardSequence: 2}));
  f.press('detail-refresh'); await flush();
  assert.match(text(f.render()), /Updated high-ID task/); assert.match(text(f.render()), /Done · Agent 2/);
  assert.doesNotMatch(text(f.render()), /New updates|no longer available/);
  assert.equal(f.commands.filter(command => command.kind === 'run').length, 1);
  assert.deepEqual(f.forbidden, []); f.stop();
});

test('a delayed preference write from an old run cannot overwrite a newer run visibility', async () => {
  const oldWrite = deferred(), f = fixture();
  f.host.store.set = async (key, value) => {
    if (value.runId === RUN) await oldWrite.promise;
    f.store.set(key, structuredClone(value));
  };
  await f.view.status(f.host, SESSION); await flush();
  f.view.begin(f.host, SESSION); await flush();
  f.press('board-hide');
  f.view.observe(f.host, runRecord({ready: {run_id: 'run-2', session_id: SESSION}}));
  await flush();
  oldWrite.resolve(); await flush();
  assert.equal(f.store.get('native-board:' + SESSION).runId, 'run-2');
  assert.equal(f.store.get('native-board:' + SESSION).hidden, true);
  assert.deepEqual(f.forbidden, []); f.stop();
});

test('explicit collection refresh includes new records and replaces only the pagination anchor', async () => {
  let latestAnchor = 99, boardSequence = 1;
  const requests = [];
  const f = fixture({read: async argv => {
    const offset = Number(argv[argv.indexOf('--offset') + 1]);
    const anchor = argv.includes('--through-sequence') ? Number(argv[argv.indexOf('--through-sequence') + 1]) : latestAnchor;
    requests.push({offset, anchor, anchored: argv.includes('--through-sequence')});
    const count = anchor === 99 && offset === 0 ? 5 : anchor === 99 && offset === 5 ? 3 : 8;
    const value = pageSnapshot(`Page ${offset}`, {offset, count, boardSequence});
    value.tasks.through_sequence = anchor;
    value.tasks.total = anchor === 99 ? 40 : anchor === 120 ? 44 : 50;
    return viewReply(value);
  }});
  await f.view.status(f.host, SESSION); await f.emit(pageSnapshot('Initial overview', {count: 5}));
  f.press('view-all-tasks'); await flush();
  f.press('next-tasks'); await flush();
  f.press('next-tasks'); await flush();
  assert.deepEqual(requests, [{offset: 0, anchor: 99, anchored: false},
    {offset: 5, anchor: 99, anchored: true}, {offset: 8, anchor: 99, anchored: true}]);
  latestAnchor = 120; boardSequence = 2;
  await f.emit(pageSnapshot('New overview', {boardSequence}));
  f.press('detail-refresh'); await flush();
  assert.deepEqual(requests.at(-1), {offset: 8, anchor: 120, anchored: false});
  assert.match(text(f.render()), /44 total/); assert.doesNotMatch(text(f.render()), /New updates/);
  f.press('next-tasks'); await flush();
  assert.deepEqual(requests.at(-1), {offset: 16, anchor: 120, anchored: true});
  f.press('previous-tasks'); await flush();
  assert.deepEqual(requests.at(-1), {offset: 8, anchor: 120, anchored: true});
  f.press('previous-tasks'); await flush();
  assert.deepEqual(requests.at(-1), {offset: 0, anchor: 120, anchored: true}, 'The old anchor\'s offset-5 history must not survive');
  f.press('board-back'); latestAnchor = 130; boardSequence = 3;
  f.press('view-all-tasks'); await flush();
  assert.deepEqual(requests.at(-1), {offset: 0, anchor: 130, anchored: false});
  assert.match(text(f.render()), /50 total/);
  assert.deepEqual(f.forbidden, []); f.stop();
});
