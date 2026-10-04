import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {setImmediate} from 'node:timers/promises';
import test from 'node:test';

const protocolSource = await readFile(new URL('../hooks/protocol.js', import.meta.url), 'utf8');
const protocolURL = 'data:text/javascript;base64,' + Buffer.from(protocolSource).toString('base64');
const protocol = await import(protocolURL);
const source = await readFile(new URL('../hooks/delm.js', import.meta.url), 'utf8');
let moduleID = 0;

const ready = {
  type: 'ready', run_id: 'native-fixture', session_id: 'session-fixture',
  token: 'private-fixture-token', socket: '/tmp/delm-fixture.sock', executable: '/tmp/delm-fixture',
  workers: [{slot: 1, cwd: '/fixture/worker-1'}, {slot: 2, cwd: '/fixture/worker-2'}], revision: 1,
};

async function fixture() {
  const hooks = [];
  const module = await import('data:text/javascript;base64,' + Buffer.from(
    source.replace("'./protocol.js'", JSON.stringify(protocolURL)) + '\n// fixture ' + moduleID++,
  ).toString('base64'));
  module.register((name, matcher, handler) => {
    hooks.push({name, matcher: typeof matcher === 'function' ? null : matcher,
      handler: typeof matcher === 'function' ? matcher : handler});
    return {catch: () => {}};
  });
  const requests = [], commands = [], timers = [], notices = [], prompts = [];
  const agents = new Map(), store = new Map();
  let queued = [{stream: 'stdout', text: JSON.stringify(ready) + '\n'}], waiting;
  const stream = {
    next: () => queued.length ? Promise.resolve({value: queued.shift(), done: false})
      : new Promise(resolve => { waiting = resolve; }),
    [Symbol.asyncIterator]() { return this; },
  };
  const host = {
    plugin: {root: '/fixture/plugin'},
    mcp: {connect: async () => ({isConnected: true, server: 'plugin:delm:delm'})},
    session: {
      id: async () => 'session-fixture', cwd: async () => '/observed/native/cwd',
      append: async input => { prompts.push(input); return {uuid: 'stored-update', message: input.message}; },
    },
    store: {get: async key => store.get(key), set: async (key, value) => { store.set(key, structuredClone(value)); }},
    fs: {read: async () => 'Shared DeLM worker contract.'},
    process: {
      spawn: () => stream,
      run: async (argv, init) => {
        const request = JSON.parse(init.stdin);
        requests.push(request);
        let result = {};
        if (request.op === 'reserve') result = {ticket: 'one-use-ticket'};
        if (request.op === 'update') result = {revision: 2};
        return {exitCode: 0, stdout: JSON.stringify({ok: true, result}), stderr: '',
          isStdoutTruncated: false, isStderrTruncated: false};
      },
    },
    command: {register: async input => { commands.push(input); }},
    clock: {after: (_, callback) => { timers.push(callback); }, sleep: async () => {}},
    ui: {log: line => { notices.push(line); }},
    prompt: {submit: async input => { prompts.push(input); }},
    agent: {list: async () => [...agents].map(([id, status]) => ({id, status}))},
    tool: {call: async input => {
      requests.push({native_tool: input.tool, id: input.task_id});
      if (agents.has(input.task_id)) agents.set(input.task_id, 'killed');
      return {result: {task_id: input.task_id}};
    }},
  };
  function hook(name, event = {}) {
    const result = hooks.find(entry => entry.name === name && (!entry.matcher
      || Object.entries(entry.matcher).every(([key, value]) => event[key] === value)));
    assert.ok(result, name);
    return result.handler;
  }
  async function call(name, event = {}, next = async input => input) {
    return hook(name, event)(host, event, next);
  }
  async function step(agentId, turnId = 'worker-turn') {
    const event = {agentId, turnId, index: 0};
    const iterator = hook('turn.step', event)(host, event, async function* () {
      yield {kind: 'text', index: 0, text: 'native'};
      return {turnId, index: 0, answer: 'native', toolUses: [], stopReason: 'end_turn', usage: null};
    });
    const chunks = [];
    for await (const chunk of iterator) chunks.push(chunk);
    return chunks;
  }
  async function launch() {
    await call('session.start', {cwd: '/fixture/project'});
    const result = await call('command.run', {command: 'delm:run', args: 'Build a useful tool.'});
    assert.match(result.args, /exactly two native fork/);
    await call('turn.start', {turnId: 'parent-turn', text: 'launch'});
  }
  async function spawn(id = 'peer-1') {
    let input;
    const result = await call('agent.spawn', {
      tool_use_id: 'spawn-' + id, fork: true, subagentType: 'fork', background: true, prompt: 'Inherited work',
    }, async event => { input = event; agents.set(id, 'running'); return {agentId: id, model: 'native-model'}; });
    return {input, result};
  }
  async function event(action) {
    const item = {stream: 'stdout', text: JSON.stringify({type: 'action', result: {actions: [action]}}) + '\n'};
    if (waiting) { const resolve = waiting; waiting = null; resolve({value: item, done: false}); }
    else queued.push(item);
    await setImmediate();
  }
  return {host, call, step, launch, spawn, event, requests, agents, timers, prompts, store, notices};
}

test('native launch contract rejects malformed workers and nonabsolute endpoints', () => {
  assert.equal(protocol.validateReady(ready), ready);
  assert.throws(() => protocol.validateReady({...ready, workers: ['/one', '/two']}));
  assert.throws(() => protocol.validateReady({...ready, revision: 0}));
  assert.throws(() => protocol.validateReady({...ready, socket: 'relative'}));
});

test('native tool outcomes preserve observed facts without inventing shell exit codes', () => {
  const result = protocol.commandOutcome({ref: 19, result: {backgroundTaskId: 'shell-1', interrupted: true}});
  assert.equal(result.result_ref, '19');
  assert.equal(result.background_task_id, 'shell-1');
  assert.equal(result.interrupted, true);
  assert.equal(Object.hasOwn(result, 'exit_code'), false);
  assert.equal(protocol.commandOutcome({deny: 'Native permission denied'}).result_ref, null);
});

test('event decoder preserves split records and refuses unbounded input', () => {
  const first = protocol.decodeLines('', '{"type":"ac');
  assert.deepEqual(first.events, []);
  assert.deepEqual(protocol.decodeLines(first.rest, 'tion"}\n').events, [{type: 'action'}]);
  assert.throws(() => protocol.decodeLines('', 'x'.repeat(2 * 1024 * 1024 + 1)));
});

test('missing native MCP tools stop before workspace preparation or model launch', async () => {
  const f = await fixture();
  f.host.mcp.connect = async () => ({isConnected: false, reason: 'failed', message: 'Native connection failed'});
  const result = await f.call('command.run', {command: 'delm:run', args: 'Do the work.'});
  assert.match(result.text, /Native connection failed/);
  assert.equal(result.exitCode, 1);
  assert.equal(f.requests.length, 0);
  assert.equal(f.store.size, 0);
});

test('only two native forks bind to separate workspaces before model execution', async () => {
  const f = await fixture(); await f.launch();
  const first = await f.spawn('peer-1'), second = await f.spawn('peer-2');
  assert.equal(first.input.cwd, ready.workers[0].cwd);
  assert.equal(second.input.cwd, ready.workers[1].cwd);
  assert.equal((await f.spawn('peer-3')).result.deny.includes('two native peers'), true);
  await f.step('peer-1');
  assert.deepEqual(f.requests.filter(r => ['bind', 'configure_scopes', 'step'].includes(r.op)).map(r => r.op),
    ['bind', 'configure_scopes', 'bind', 'configure_scopes', 'step']);
  const scope = f.requests.find(r => r.op === 'configure_scopes');
  assert.deepEqual(scope.scopes, [{path: '/fixture/worker-1', access: 'write'}]);
});

test('a parent ending without both native peers cancels the incomplete launch', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  await f.call('turn.complete', {turnId: 'parent-turn', reason: 'answer', isAborted: false, answer: 'Launched.'});
  assert.ok(f.requests.some(request => request.op === 'cancel' && /two-peer/.test(request.reason)));
  assert.ok(f.notices.some(notice => /two-peer/.test(notice)));
  const normal = await fixture(); await normal.launch(); await normal.spawn('peer-1'); await normal.spawn('peer-2');
  await normal.call('turn.complete', {turnId: 'parent-turn', reason: 'answer', isAborted: false, answer: 'Launched.'});
  assert.equal(normal.requests.some(request => request.op === 'cancel'), false);
});

test('coordination tickets pass through native permission middleware and cannot be forged', async () => {
  const f = await fixture(); await f.launch(); await f.spawn(); await f.step('peer-1');
  const event = {tool: protocol.TOOL_PREFIX + 'delm_status', agentId: 'peer-1', tool_use_id: 'call-1'};
  let observed;
  const nativeDenial = {deny: 'Native permissions refused this call'};
  const result = await f.call('tool.call', event, async input => { observed = input; return nativeDenial; });
  assert.equal(result, nativeDenial);
  assert.deepEqual(observed._delm, {socket: ready.socket, ticket: 'one-use-ticket'});
  assert.equal(f.requests.at(-1).op, 'revoke');
  const count = f.requests.length;
  const forged = await f.call('tool.call', {...event, _delm: {ticket: 'forged'}});
  assert.match(forged.deny, /credentials/);
  assert.equal(f.requests.length, count);
  assert.match((await f.call('tool.call', {...event, agentId: 'unknown'})).deny, /bind/);
});

test('Bash evidence uses observed cwd, preserves the native result, and does not qualify denied execution', async () => {
  const f = await fixture(); await f.launch(); await f.spawn(); await f.step('peer-1');
  const event = {tool: 'Bash', agentId: 'peer-1', tool_use_id: 'bash-1', command: 'node --test'};
  const native = {ref: 8, result: {stdout: 'passed', interrupted: false}};
  assert.equal(await f.call('tool.call', event, async () => native), native);
  assert.equal(f.requests.find(r => r.op === 'command_start').cwd, '/observed/native/cwd');
  assert.equal(f.requests.find(r => r.op === 'command_end').result_ref, '8');
  const endCount = f.requests.filter(r => r.op === 'command_end').length;
  await f.call('tool.call', {...event, tool_use_id: 'denied'}, async () => ({deny: 'Denied'}));
  assert.equal(f.requests.filter(r => r.op === 'command_end').length, endCount);
});

test('identical resume requests on different worker turns are both delivered', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  const resume = {type: 'resume', agent_id: 'peer-1', revision: 1, message: 'Continue the remaining work.'};
  await f.event(resume);
  await f.timers.shift()();
  await f.call('session.send', {to: 'peer-1', text: 'DeLM task revision 1:\nContinue the remaining work.', origin: {kind: 'model'}},
    async () => ({isDelivered: true}));
  await f.event(resume);
  await f.timers.shift()();
  assert.equal(f.prompts.length, 2);
  assert.match(f.prompts[0].text, /normal native SendMessage/);
});

test('an update stored during a captured step stops that stale request and resumes before acknowledgment', async () => {
  const f = await fixture(); await f.launch(); await f.spawn(); await f.step('peer-1');
  await f.call('prompt.submit', {text: 'Change the output.', origin: {kind: 'composer'}});
  let stored;
  f.host.session.append = input => new Promise(resolve => { stored = () => {
    f.prompts.push(input); resolve({uuid: 'stored', message: input.message});
  }; });
  await f.event({type: 'context', agent_id: 'peer-1', revision: 2, message: 'Change the output.'});
  let stepped = false;
  const step = f.step('peer-1').then(() => { stepped = true; });
  await setImmediate();
  assert.equal(stepped, false);
  assert.equal(f.requests.filter(r => r.op === 'step').at(-1).revision, 1);
  stored();
  await step;
  assert.equal(f.prompts.length, 1);
  assert.equal(f.timers.length, 0);
  assert.equal(f.requests.filter(r => r.op === 'step').at(-1).revision, 1);
  await f.call('turn.complete', {agentId: 'peer-1', turnId: 'worker-turn', reason: 'answer',
    isAborted: false, answer: 'DeLM is resuming this peer with the updated task context.'});
  assert.equal(f.timers.length, 1);
  const exact = f.store.get('native-run:session-fixture').agents['peer-1'].pending.message;
  await f.call('session.send', {to: 'peer-1', text: exact, origin: {kind: 'model'}},
    async () => ({isDelivered: true}));
  await f.step('peer-1', 'fresh-resumed-turn');
  assert.equal(f.requests.filter(r => r.op === 'step').at(-1).revision, 2);
});

test('ended peers acknowledge only confirmed exact native SendMessage delivery', async () => {
  const f = await fixture(); await f.launch(); await f.spawn(); await f.step('peer-1');
  await f.call('prompt.submit', {text: 'New request.', origin: {kind: 'composer'}});
  await f.event({type: 'resume', agent_id: 'peer-1', revision: 2, message: 'New request.'});
  const wrong = await f.call('session.send', {to: 'peer-1', text: 'Paraphrase', origin: {kind: 'model'}});
  assert.equal(wrong.isDelivered, false);
  const sent = await f.call('session.send', {to: 'peer-1', text: 'DeLM task revision 2:\nNew request.', origin: {kind: 'model'}},
    async () => ({isDelivered: true}));
  assert.equal(sent.isDelivered, true);
  await f.step('peer-1', 'resumed-turn');
  assert.equal(f.requests.filter(r => r.op === 'step').at(-1).revision, 2);
});

test('even an already-stored active update requires a fresh native turn before acknowledgment', async () => {
  const f = await fixture(); await f.launch(); await f.spawn(); await f.step('peer-1');
  await f.call('prompt.submit', {text: 'Update before the hook starts.', origin: {kind: 'composer'}});
  await f.event({type: 'context', agent_id: 'peer-1', revision: 2, message: 'Update before the hook starts.'});
  const chunks = await f.step('peer-1');
  assert.match(chunks.find(chunk => chunk.kind === 'text').text, /resuming/);
  assert.equal(f.requests.filter(request => request.op === 'step').at(-1).revision, 1);
  assert.equal(f.store.get('native-run:session-fixture').agents['peer-1'].deliveredRevision, 2);
  assert.equal(f.store.get('native-run:session-fixture').agents['peer-1'].acknowledgedRevision, 1);
});

test('a peer bound after an update starts with inherited revision and catches up before its first model call', async () => {
  const f = await fixture(); await f.launch();
  await f.call('prompt.submit', {text: 'Update during launch.', origin: {kind: 'composer'}});
  await f.spawn();
  assert.equal(f.store.get('native-run:session-fixture').agents['peer-1'].deliveredRevision, 1);
  await f.event({type: 'context', agent_id: 'peer-1', revision: 2, message: 'Complete current task with the launch update.'});
  const chunks = await f.step('peer-1', 'stale-first-turn');
  assert.match(chunks.find(chunk => chunk.kind === 'text').text, /resuming/);
  assert.equal(f.requests.some(request => request.op === 'step'), false);
  await f.call('turn.complete', {agentId: 'peer-1', turnId: 'stale-first-turn', reason: 'answer', isAborted: false, answer: 'Resuming.'});
  assert.equal(f.requests.some(request => request.op === 'turn_end'), false);
  const exact = f.store.get('native-run:session-fixture').agents['peer-1'].pending.message;
  await f.call('session.send', {to: 'peer-1', text: exact, origin: {kind: 'model'}}, async () => ({isDelivered: true}));
  await f.step('peer-1', 'fresh-first-turn');
  assert.equal(f.requests.find(request => request.op === 'step').revision, 2);
});

test('native task registry lag after TaskStop is polled before settlement', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  let reads = 0;
  f.host.agent.list = async () => [{id: 'peer-1', status: ++reads < 4 ? 'running' : 'killed'}];
  await f.event({type: 'candidate', agent_id: 'peer-1'});
  await f.timers.shift()();
  assert.ok(reads >= 4);
  assert.ok(f.requests.some(r => r.op === 'settle'));
  assert.equal(f.notices.length, 0);
});

test('settlement stops only owned peers and background shells before delivery', async () => {
  const f = await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2');
  f.agents.set('unrelated-agent', 'running');
  await f.step('peer-1');
  await f.call('tool.call', {tool: 'Bash', agentId: 'peer-1', tool_use_id: 'background', command: 'serve'},
    async () => ({ref: 11, result: {backgroundTaskId: 'owned-shell'}}));
  f.agents.set('peer-1', 'completed');
  await f.event({type: 'candidate', agent_id: 'peer-1'});
  await f.timers.shift()();
  assert.equal(f.agents.get('unrelated-agent'), 'running');
  assert.deepEqual(f.requests.filter(r => r.native_tool).map(r => r.id), ['peer-2', 'owned-shell']);
  const settle = f.requests.find(r => r.op === 'settle');
  assert.equal(settle.background_tasks_stopped, true);
  assert.deepEqual(settle.agents, [{id: 'peer-1', status: 'completed'}, {id: 'peer-2', status: 'killed'}]);
});

test('native stop snapshot proves natural shell completion and final delivery can require a focused check', async () => {
  const f = await fixture(); await f.launch(); await f.spawn(); await f.step('peer-1');
  await f.call('tool.call', {tool: 'Bash', agentId: 'peer-1', tool_use_id: 'background', command: 'check'},
    async () => ({ref: 11, result: {backgroundTaskId: 'completed-shell'}}));
  await f.call('classic.SubagentStop', {agent_id: 'peer-1', background_tasks: []});
  const saved = f.store.get('native-run:session-fixture');
  assert.equal(saved.background['completed-shell'].stopped, true);
  await f.event({type: 'final', status: 'delivered', delivery: {verification_required: true}});
  await f.timers.shift()();
  assert.match(f.prompts[0].text, /focused check/);
  assert.match(f.prompts[0].text, /Do not repeat unaffected checks/);
});
