import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {setImmediate} from 'node:timers/promises';
import test from 'node:test';

const protocolSource = await readFile(new URL('../hooks/protocol.js', import.meta.url), 'utf8');
const protocolURL = 'data:text/javascript;base64,' + Buffer.from(protocolSource).toString('base64');
const protocol = await import(protocolURL);
const rendererSource = await readFile(new URL('../hooks/board-render.js', import.meta.url), 'utf8');
const rendererURL = 'data:text/javascript;base64,' + Buffer.from(rendererSource).toString('base64');
const boardSource = await readFile(new URL('../hooks/board-view.js', import.meta.url), 'utf8');
const boardURL = 'data:text/javascript;base64,' + Buffer.from(
  boardSource.replace("'./board-render.js'", JSON.stringify(rendererURL)),
).toString('base64');
const source = await readFile(new URL('../hooks/delm.js', import.meta.url), 'utf8');
let moduleID = 0;

const ready = {
  type: 'ready', run_id: 'native-fixture', session_id: 'session-fixture',
  token: 'private-fixture-token', socket: '/tmp/delm-fixture.sock', executable: '/tmp/delm-fixture',
  workers: [{slot: 1, cwd: '/fixture/worker-1'}, {slot: 2, cwd: '/fixture/worker-2'}], revision: 1,
};

async function fixture(options = {}) {
  const hooks = [];
  const module = await import('data:text/javascript;base64,' + Buffer.from(
    source.replace("'./protocol.js'", JSON.stringify(protocolURL))
      .replace("'./board-view.js'", JSON.stringify(boardURL)) + '\nexport {retryFinish};\n// fixture ' + moduleID++,
  ).toString('base64'));
  module.register((name, matcher, handler) => {
    hooks.push({name, matcher: typeof matcher === 'function' ? null : matcher,
      handler: typeof matcher === 'function' ? matcher : handler});
    return {catch: () => {}};
  });
  const requests = [], commands = [], timers = [], notices = [], prompts = [];
  const agents = new Map(), store = options.store || new Map();
  let session = options.session || 'session-fixture', revision = 1;
  let finalization = null, generation = 0;
  const streams = [];
  function createStream() {
    const state = {queued: [{stream: 'stdout', text: JSON.stringify({...ready, session_id: session}) + '\n'}], waiting: null};
    streams.push(state);
    return {
      next: () => state.queued.length ? Promise.resolve({value: state.queued.shift(), done: false})
        : new Promise(resolve => { state.waiting = resolve; }),
      [Symbol.asyncIterator]() { return this; },
    };
  }
  const host = {
    plugin: {name: 'delm', root: '/fixture/plugin'},
    mcp: {connect: async () => ({isConnected: true, server: 'plugin:delm:delm'})},
    session: {
      id: async () => session, cwd: async () => '/observed/native/cwd',
      append: async input => { prompts.push(input); return {uuid: 'stored-update', message: input.message}; },
    },
    store: {get: async key => store.get(key), set: async (key, value) => { store.set(key, structuredClone(value)); }},
    fs: {read: async () => 'Shared DeLM worker contract.'},
    process: {
      spawn: () => createStream(),
      run: async (argv, init) => {
        const request = JSON.parse(init.stdin);
        requests.push(argv.includes('recover') ? {...request, native_recover: true} : request);
        if (argv.includes('recover')) return {exitCode: 0, stdout: JSON.stringify({status: 'interrupted', message: 'Saved work is recoverable.'}), stderr: ''};
        let result = {};
        if (request.op === 'reserve') result = {ticket: 'one-use-ticket'};
        if (request.op === 'update') {
          if (finalization?.started || finalization?.intent === 'cancel') {
            return {exitCode:0, stdout:JSON.stringify({ok:false,error:'DeLM is finishing. This update was not delivered.'}),stderr:''};
          }
          finalization = null;
          result = {revision: ++revision};
        }
        if (request.op === 'begin_settle') {
          if (!finalization || request.generation !== finalization.generation || request.revision !== revision) {
            return {exitCode:0,stdout:JSON.stringify({ok:false,error:'Finalization is obsolete; refresh the run state'}),stderr:''};
          }
          finalization.started = true;
          result = {finalization:{...finalization}};
        }
        if (request.op === 'cancel') {
          finalization = {generation:++generation,revision,intent:'cancel',reason:'user_cancelled',started:false};
          result = {actions:[{type:'stop',finalization:{...finalization}}]};
        }
        if (request.op === 'status') {
          if (options.store && !options.liveBridge) return {exitCode:1,stdout:'',stderr:'DeLM is no longer running'};
          result = {run: {...ready, status: 'running', finalization}, board: {tasks: []}};
        }
        return {exitCode: 0, stdout: JSON.stringify({ok: true, result}), stderr: '',
          isStdoutTruncated: false, isStderrTruncated: false};
      },
    },
    command: {register: async input => { commands.push(input); }},
    clock: {after: (milliseconds, callback) => {
      if (milliseconds <= 1000) timers.push(callback);
      return {cancel() {}};
    }, every: () => ({cancel() {}}), sleep: async () => {}},
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
    const found = hooks.filter(entry => (entry.name === name
      || (entry.name.endsWith('.*') && name.startsWith(entry.name.slice(0, -1)))) && (!entry.matcher
      || Object.entries(entry.matcher).every(([key, value]) => event[key] === value)));
    assert.ok(found.length, name);
    return ($, e, final) => {
      const dispatch = (index, current) => {
        if (index === found.length) return final(current);
        const next = input => dispatch(index + 1, input);
        next.is = pattern => pattern === name;
        return found[index].handler($, current, next);
      };
      return dispatch(0, e);
    };
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
  async function event(action, streamIndex = streams.length - 1) {
    if (['candidate','stop'].includes(action.type) && !action.finalization) {
      finalization = {generation:++generation,revision,intent:action.type==='candidate'?'deliver':'cancel',reason:'fixture',started:false};
      action = {...action,finalization:{...finalization}};
    }
    const item = {stream: 'stdout', text: JSON.stringify({type: 'action', result: {actions: [action]}}) + '\n'};
    const state = streams[streamIndex];
    if (state.waiting) { const resolve = state.waiting; state.waiting = null; resolve({value: item, done: false}); }
    else state.queued.push(item);
    await setImmediate();
  }
  return {host, call, step, launch, spawn, event, requests, agents, timers, prompts, store, notices,
    retryFinish: () => module.retryFinish(host,session), select: id => { session = id; }};
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
  assert.equal([...f.store.keys()].some(key => key.startsWith('native-run:')), false);
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

test('a ready peer begins while the other native binding is still pending', async () => {
  const f=await fixture(); await f.launch(); await f.spawn('peer-1');
  const nativeRun=f.host.process.run;
  let release;
  f.host.process.run=async(argv,init)=>{
    const request=JSON.parse(init.stdin);
    if(request.op==='bind' && request.agent_id==='peer-2') await new Promise(resolve=>{release=resolve;});
    return nativeRun(argv,init);
  };
  const second=f.spawn('peer-2'); await setImmediate();
  let secondStepped=false;
  const waiting=f.step('peer-2').then(result=>{secondStepped=true;return result;});
  assert.equal((await f.step('peer-1'))[0].text,'native');
  assert.equal(secondStepped,false);
  assert.equal(f.requests.filter(r=>r.op==='step' && r.agent_id==='peer-1').length,1);
  assert.equal(f.requests.filter(r=>r.op==='step' && r.agent_id==='peer-2').length,0);
  release(); await second; await waiting;
  assert.equal(secondStepped,true);
  assert.equal(f.requests.filter(r=>r.op==='step' && r.agent_id==='peer-2').length,1);
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
  assert.deepEqual(f.requests.filter(r => r.native_tool).map(r => r.id), ['owned-shell', 'peer-2']);
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


test('conversation controls reselect by native session identity without session.start', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  f.select('new-conversation');
  assert.match((await f.call('command.run', {command: 'delm-status'})).text, /No DeLM run/);
  assert.match((await f.call('command.run', {command: 'delm-stop'})).text, /No DeLM run/);
  const before = f.requests.length;
  await f.call('prompt.submit', {text: 'Unrelated request.', origin: {kind: 'composer'}});
  assert.equal(f.requests.length, before);
  assert.match((await f.call('tool.call', {tool: 'Bash', agentId: 'peer-1', command: 'write'})).deny, /conversation has ended/);
  f.select('session-fixture');
  assert.match((await f.call('command.run', {command: 'delm-status'})).text, /DeLM/);
  assert.equal(f.requests.some(item => item.op === 'status'), false);
});

test('clear, resume, and branch suppress delayed controls and recover only the owning conversation', async () => {
  for (const reason of ['clear', 'resume']) {
    const f = await fixture(); await f.launch(); await f.spawn();
    await f.event({type: 'resume', agent_id: 'peer-1', revision: 1, message: 'Continue.'});
    await f.call('session.end', {sessionId: 'session-fixture', reason});
    f.select('destination');
    await f.timers.shift()();
    assert.equal(f.prompts.length, 0);
    assert.ok(f.requests.some(item => item.op === 'cancel' && item.reason.endsWith(reason)));
    assert.match((await f.call('command.run', {command: 'delm-status'})).text, /No DeLM run/);
    assert.equal(f.requests.some(item => item.native_recover), false);
    f.select('session-fixture');
    const status = await f.call('command.run', {command: 'delm-status'});
    assert.match(status.text, /DeLM/);
    assert.equal(f.requests.filter(item => item.native_recover).length, 0);
    await f.call('session.start');
    await setImmediate();
    assert.equal(f.requests.filter(item => item.op === 'settle').length, 1);
    await f.call('command.run', {command: 'delm-status'});
    assert.equal(f.requests.filter(item => item.op === 'settle').length, 1);
  }
});

test('a final report queued before a conversation switch cannot enter the new conversation', async () => {
  const f = await fixture(); await f.launch();
  await f.event({type: 'final', status: 'delivered', delivery: {verification_required: false}});
  f.select('another-session');
  await f.timers.shift()();
  assert.equal(f.prompts.length, 0);
  f.select('session-fixture');
  assert.match((await f.call('command.run', {command: 'delm-status'})).text, /DeLM/);
  assert.equal(f.store.get('native-run:session-fixture').final.status, 'delivered');
});

test('reload recovery retains failure and never sends controls into an unrelated conversation', async () => {
  const first = await fixture(); await first.launch(); await first.spawn();
  const reload = await fixture({store: first.store});
  await reload.call('session.start');
  await setImmediate();
  assert.equal(reload.requests.some(item => item.native_recover), false);
  assert.ok(reload.notices.some(text => /shutdown is not confirmed/iu.test(text)));
  assert.equal(reload.store.get('native-run:session-fixture').finished, false);
  reload.select('unrelated');
  assert.match((await reload.call('command.run', {command: 'delm-stop'})).text, /No DeLM run/);
});

test('unsupported follow-up attachments never advance the task or start a parent response', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  for (const type of ['image', 'document', 'audio']) {
    let entered = false;
    const result = await f.call('prompt.submit', {text: 'Use this.', origin: {kind: 'composer'}, attachments: [{type, filename: 'input'}]},
      async () => { entered = true; });
    assert.match(result.drop, new RegExp(type));
    assert.match(result.drop, /previous task/);
    assert.equal(entered, false);
  }
  assert.equal(f.requests.some(item => item.op === 'update'), false);
  assert.equal(f.store.get('native-run:session-fixture').revision, 1);
  // An attachment on the initial skill submission stays with the native fork.
  const initial = await fixture();
  const input = {text: '/delm:run Read this image.', origin: {kind: 'composer'}, attachments: [{type: 'image'}]};
  assert.deepEqual(await initial.call('prompt.submit', input), input);
});

test('unexpanded references explain the limitation while emails, quoted values, and code remain ordinary text', () => {
  for (const text of ['Use @README.md', 'Follow @"design notes.md"', 'Look at @src/main.js', 'Use @AGENTS', "Don't modify @README.md because it's needed."]) {
    assert.throws(() => protocol.followupText({text}), /@references/);
  }
  for (const text of ['Email help@example.com', 'Use `@decorator`', 'Run ```js\n@decorator\n```', 'Set "@scope/package" as the name.', "Set '@scope/package' as the name.", "Don't rename it; it's public."]) {
    assert.equal(protocol.followupText({text}), text);
  }
});

test('accepted prompt rewrites and additional context reach the runtime without DeLM control prose', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  await f.call('prompt.submit', {text: 'Raw request', context: ['Selected source text'], origin: {kind: 'composer'}},
    async input => ({...input, text: 'Accepted request', context: [...input.context, 'Organization context']}));
  const update = f.requests.find(item => item.op === 'update');
  assert.equal(update.text, 'Accepted request\n\nAdditional context:\nSelected source text\n\nOrganization context');
  assert.equal(update.text.includes('lightweight control'), false);
});

test('native middleware rejection does not advance revision or notify peers', async () => {
  const f = await fixture(); await f.launch();
  const result = await f.call('prompt.submit', {text: 'Blocked update', origin: {kind: 'composer'}}, async () => ({drop: 'Native policy rejected'}));
  assert.deepEqual(result, {drop: 'Native policy rejected'});
  assert.equal(f.requests.some(item => item.op === 'update'), false);
});

test('both peers receive one complete context update before acknowledging its revision', async () => {
  const f = await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2');
  await f.call('prompt.submit', {text: 'New task', context: ['Selected text'], origin: {kind: 'composer'}});
  const text = f.requests.find(item => item.op === 'update').text;
  for (const id of ['peer-1', 'peer-2']) {
    const action = {type: 'context', agent_id: id, revision: 2, message: text};
    await f.event(action); await f.event(action);
  }
  assert.equal(f.prompts.length, 2);
  assert.equal(f.prompts.every(item => item.message.content[0].text.includes('Selected text')), true);
  assert.equal(f.requests.some(item => item.op === 'step' && item.revision === 2), false);
});

test('native context refusal or truncation cannot acknowledge delivery', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  await f.call('prompt.submit', {text: 'Important details', origin: {kind: 'composer'}});
  f.host.session.append = async input => ({uuid: 'rewritten', message: {...input.message, content: [{type: 'text', text: 'truncated'}]}});
  await f.event({type: 'context', agent_id: 'peer-1', revision: 2, message: 'Important details'});
  assert.equal(f.store.get('native-run:session-fixture').agents['peer-1'].deliveredRevision, 1);
  assert.match(f.notices.at(-1), /complete task update/);
  const stopped = await f.step('peer-1');
  assert.match(stopped[0].text, /recovering/);
  assert.equal(f.requests.some(item => item.op === 'step'), false);
});

test('fork middleware preserves native setup fields and changes only workspace and background scheduling', async () => {
  const f = await fixture(); await f.launch();
  const original = {fork: true, subagentType: 'fork', tool_use_id: 'native-fork', prompt: 'Inherited context',
    parentModel: 'parent-model', permissionMode: 'auto', provider: {plugin: 'engine', tier: 'core'},
    description: 'Peer', background: false};
  let received;
  await f.call('agent.spawn', original, async input => { received = input; return {agentId: 'native-peer'}; });
  assert.deepEqual(received, {...original, cwd: ready.workers[0].cwd, background: true});
  assert.equal(Object.hasOwn(received, 'model'), false);
  assert.equal(Object.hasOwn(received, 'isolation'), false);
});


test('newer pending revisions suppress older context and obsolete resume timers', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  await f.event({type: 'resume', agent_id: 'peer-1', revision: 2, message: 'First update.'});
  await f.event({type: 'resume', agent_id: 'peer-1', revision: 3, message: 'Newer update.'});
  await f.event({type: 'context', agent_id: 'peer-1', revision: 2, message: 'Older context.'});
  assert.equal(f.prompts.length, 0);
  await f.timers.shift()();
  assert.equal(f.prompts.length, 0);
  await f.timers.shift()();
  assert.equal(f.prompts.length, 1);
  assert.match(f.prompts[0].text, /Newer update/);
  assert.equal(f.store.get('native-run:session-fixture').agents['peer-1'].pending.revision, 3);
});

test('unrelated native agents continue normally while a DeLM update needs attention', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  await f.call('prompt.submit', {text: 'Update', origin: {kind: 'composer'}});
  f.host.session.append = async () => ({deny: 'Native policy refused context'});
  await f.event({type: 'context', agent_id: 'peer-1', revision: 2, message: 'Update'});
  const unrelated = await f.step('unrelated-agent');
  assert.equal(unrelated[0].text, 'native');
});


test('parallel slash invocations prepare only one native runtime', async () => {
  const f = await fixture();
  let connected;
  f.host.mcp.connect = () => new Promise(resolve => { connected = resolve; });
  const first = f.call('command.run', {command: 'delm:run', args: 'First request'});
  await setImmediate();
  const second = await f.call('command.run', {command: 'delm:run', args: 'Second request'});
  assert.match(second.text, /preparing/);
  connected({isConnected: true});
  assert.match((await first).args, /First request/);
});

test('conversation end while preparation waits cannot admit workers even before session ID changes', async () => {
  const f = await fixture();
  let connected;
  f.host.mcp.connect = () => new Promise(resolve => { connected = resolve; });
  const starting = f.call('command.run', {command: 'delm:run', args: 'Start task'});
  await setImmediate();
  await f.call('session.end', {sessionId: 'session-fixture', reason: 'clear'});
  connected({isConnected: true});
  const result = await starting;
  assert.equal(result.exitCode, 1);
  assert.ok(f.requests.some(item => item.op === 'cancel'));
  assert.equal(f.store.get('native-run:session-fixture').ending, true);
});

test('a missing saved run is not cached over a subsequent restored record', async () => {
  const f = await fixture();
  await f.call('command.run', {command: 'delm-status'});
  f.store.set('native-run:session-fixture', {session: 'session-fixture', finished: true, final: {status: 'delivered'}});
  assert.match((await f.call('command.run', {command: 'delm-status'})).text, /DeLM/);
  assert.equal(f.requests.some(item => item.op === 'status' || item.native_recover), false);
});

test('session switch while a native append waits cannot acknowledge old worker delivery', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  await f.call('prompt.submit', {text: 'New requirement', origin: {kind: 'composer'}});
  let appended;
  f.host.session.append = input => new Promise(resolve => { appended = () => resolve({uuid: 'stored', message: input.message}); });
  await f.event({type: 'context', agent_id: 'peer-1', revision: 2, message: 'New requirement'});
  f.select('other');
  appended();
  await setImmediate();
  assert.equal(f.store.get('native-run:session-fixture').agents['peer-1'].deliveredRevision, 1);
  assert.equal(f.notices.length, 0);
});

test('session switch while native step admission waits cannot start the model', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  const original = f.host.process.run;
  let admitted;
  f.host.process.run = async (argv, input) => {
    if (JSON.parse(input.stdin).op === 'step') await new Promise(resolve => { admitted = resolve; });
    return original(argv, input);
  };
  const stepping = f.step('peer-1');
  await setImmediate();
  f.select('other');
  admitted();
  const chunks = await stepping;
  assert.match(chunks[0].text, /paused/);
  assert.equal(f.store.get('native-run:session-fixture').agents['peer-1'].acknowledgedRevision, 0);
});

test('a failed update rejects only that submission and releases later ordinary conversation', async () => {
  const f = await fixture(); await f.launch(); await f.spawn();
  const original = f.host.process.run;
  f.host.process.run = async (argv, input) => JSON.parse(input.stdin).op === 'update'
    ? {exitCode: 1, stdout: '', stderr: 'Bridge unavailable'} : original(argv, input);
  const rejected = await f.call('prompt.submit', {text: 'New instruction', origin: {kind: 'composer'}});
  assert.match(rejected.drop, /could not confirm delivery/);
  const requests = f.requests.length;
  for (const text of ['Investigate it yourself.', 'Now write a report.']) {
    const input = {text, origin:{kind:'composer'}};
    assert.deepEqual(await f.call('prompt.submit', input), input);
    assert.equal((await f.step(undefined, 'ordinary-turn'))[0].text, 'native');
  }
  assert.equal(f.requests.length, requests);
});

test('late native background proof retries failed settlement without cancelling its candidate', async () => {
  const f=await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2'); await f.step('peer-1');
  await f.call('tool.call',{tool:'Bash',agentId:'peer-1',tool_use_id:'bg',command:'build'},
    async()=>({ref:11,result:{backgroundTaskId:'completed-shell'}}));
  f.agents.set('peer-1','completed'); f.agents.set('peer-2','completed');
  f.host.tool.call=async()=>({isError:true,result:'No running task'});
  await f.event({type:'candidate',agent_id:'peer-1'});
  await f.timers.shift()();
  assert.equal(f.requests.some(r=>r.op==='settle'),false);
  assert.equal(f.store.get('native-run:session-fixture').canRetryFinish,true);
  assert.equal(f.store.get('native-run:session-fixture').finalization.intent,'deliver');
  await f.call('classic.SubagentStop',{agent_id:'peer-1',background_tasks:[]});
  await f.timers.shift()();
  assert.equal(f.requests.filter(r=>r.op==='settle').length,1);
  assert.equal(f.requests.some(r=>r.op==='cancel'),false);
});

test('long background work is stopped before its parent can cascade and retire its acknowledgment', async () => {
  const f=await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2'); await f.step('peer-2');
  await f.call('tool.call',{tool:'Bash',agentId:'peer-2',tool_use_id:'long-wait',command:'sleep 300'},
    async()=>({ref:31,result:{backgroundTaskId:'long-shell'}}));
  let shellRunning=true;
  const calls=[];
  f.host.tool.call=async input=>{
    calls.push(input.task_id);
    if(input.task_id==='long-shell') {
      if(!shellRunning) return {isError:true,result:'Task not found'};
      shellRunning=false;
    } else {
      if(input.task_id==='peer-2') shellRunning=false;
      f.agents.set(input.task_id,'killed');
    }
    return {result:{task_id:input.task_id}};
  };
  await f.event({type:'candidate',agent_id:'peer-1'}); await f.timers.shift()();
  assert.deepEqual(calls,['long-shell','peer-1','peer-2']);
  assert.equal(f.requests.filter(r=>r.op==='settle').length,1);
});

test('native terminal notification reconciles a retired shell independently of board visibility', async () => {
  const f=await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2'); await f.step('peer-1');
  await f.call('tool.call',{tool:'Bash',agentId:'peer-1',tool_use_id:'shell-start',command:'sleep 300'},
    async()=>({ref:32,result:{backgroundTaskId:'shell-finished'}}));
  f.agents.set('peer-1','completed'); f.agents.set('peer-2','killed');
  f.host.tool.call=async()=>({isError:true,result:'No running task'});
  await f.event({type:'candidate',agent_id:'peer-1'}); await f.timers.shift()();
  assert.equal(f.requests.some(r=>r.op==='settle'),false);
  const draw={component:'UserMessage',surface:'terminal',props:{origin:{kind:'task-notification'},
    task:{id:'shell-finished',status:'killed',toolUseId:'shell-start'},text:'Native notification'}};
  const rendered={native:true};
  assert.equal(await f.call('ui.render',draw,async()=>rendered),rendered);
  // No board render/open command is involved in lifecycle observation.
  assert.equal(f.store.get('native-run:session-fixture').background['shell-finished'].stopped,false);
  while(f.timers.length) await f.timers.shift()();
  assert.equal(f.store.get('native-run:session-fixture').background['shell-finished'].proof.source,'native-task-notification');
  assert.equal(f.requests.some(r=>r.op==='settle'),true);
});

test('notification-like text, foreign IDs, mismatched calls and nonterminal statuses prove no shutdown', async () => {
  const f=await fixture(); await f.launch(); await f.spawn(); await f.step('peer-1');
  await f.call('tool.call',{tool:'Bash',agentId:'peer-1',tool_use_id:'shell-start',command:'build'},
    async()=>({ref:33,result:{backgroundTaskId:'owned-shell'}}));
  for(const props of [
    {origin:{kind:'composer'},task:{id:'owned-shell',status:'killed'}},
    {origin:{kind:'task-notification'},task:{id:'foreign-shell',status:'killed'}},
    {origin:{kind:'task-notification'},task:{id:'owned-shell',status:'killed',toolUseId:'another-call'}},
    {origin:{kind:'task-notification'},task:{id:'owned-shell',status:'running'}},
    {origin:{kind:'task-notification'},text:'owned-shell [killed]'},
  ]) await f.call('ui.render',{component:'UserMessage',surface:'terminal',props});
  assert.equal(f.timers.length,0);
  assert.equal(f.store.get('native-run:session-fixture').background['owned-shell'].stopped,false);
});

test('already queued plugin resume cannot start a model turn after another worker completes', async () => {
  const f=await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2');
  f.host.prompt.submit=async input=>{
    f.prompts.push(input);
    return f.call('prompt.submit',{...input,origin:{kind:'plugin',name:'delm'}});
  };
  await f.event({type:'resume',agent_id:'peer-1',revision:1,message:'Continue remaining work.'});
  await f.timers.shift()();
  assert.equal(f.prompts.length,1);
  await f.event({type:'candidate',agent_id:'peer-2'});
  await f.call('turn.start',{turnId:'obsolete-resume',text:f.prompts[0].text});
  const chunks=await f.step(undefined,'obsolete-resume');
  assert.equal(chunks.some(chunk=>chunk.text==='native'),false);
  const ordinary={text:'Explain the current state.',origin:{kind:'composer'}};
  assert.deepEqual(await f.call('prompt.submit',ordinary),ordinary);
  await f.call('turn.start',{turnId:'ordinary',text:ordinary.text});
  assert.equal((await f.step(undefined,'ordinary'))[0].text,'native');
});

test('ordinary parent turn does not wait behind an old in-flight update after run ownership closes', async () => {
  const f=await fixture(); await f.launch(); await f.spawn();
  const nativeRun=f.host.process.run;
  let release;
  f.host.process.run=async(argv,init)=>{
    const request=JSON.parse(init.stdin);
    if(request.op==='update') return new Promise(resolve=>{release=()=>resolve({exitCode:1,stdout:'',stderr:'Bridge exited'});});
    return nativeRun(argv,init);
  };
  const pending=f.call('prompt.submit',{text:'A worker update.',origin:{kind:'composer'}});
  await setImmediate();
  await f.event({type:'candidate',agent_id:'peer-1'});
  const chunks=await Promise.race([f.step(undefined,'new-ordinary-turn'),new Promise((_,reject)=>setTimeout(()=>reject(new Error('Ordinary turn blocked by old update')),100))]);
  assert.equal(chunks[0].text,'native');
  release(); await pending;
});

test('complete declaration holds queued resume before candidate capture and never stops peers early', async () => {
  const f=await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2');
  f.host.prompt.submit=async input=>{f.prompts.push(input);return f.call('prompt.submit',{...input,origin:{kind:'plugin',name:'delm'}});};
  await f.event({type:'resume',agent_id:'peer-1',revision:1,message:'Continue remaining work.'});
  await f.timers.shift()();
  await f.event({type:'completion_intent',agent_id:'peer-2',revision:1,turn_id:'turn-two',pending:true});
  await f.call('turn.start',{turnId:'held-resume',text:f.prompts[0].text});
  assert.equal((await f.step(undefined,'held-resume')).some(chunk=>chunk.text==='native'),false);
  assert.equal(f.requests.some(r=>r.native_tool==='TaskStop'),false);
  assert.equal(f.requests.some(r=>r.op==='begin_settle'),false);
  await f.event({type:'candidate',agent_id:'peer-2'});
  await f.event({type:'completion_intent',agent_id:'peer-2',revision:1,turn_id:'turn-two',pending:false});
  while(f.timers.length) await f.timers.shift()();
  assert.equal(f.prompts.length,1);
  assert.equal(f.requests.filter(r=>r.op==='settle').length,1);
});

test('failed or withdrawn completion releases held ready work immediately without polling', async () => {
  for(const reason of ['failed','withdrawn']) {
    const f=await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2');
    f.host.prompt.submit=async input=>{f.prompts.push(input);return f.call('prompt.submit',{...input,origin:{kind:'plugin',name:'delm'}});};
    await f.event({type:'completion_intent',agent_id:'peer-2',revision:1,turn_id:'turn-two',pending:true});
    await f.event({type:'resume',agent_id:'peer-1',revision:1,message:'Ready work after '+reason});
    await f.timers.shift()();
    assert.equal(f.prompts.length,0);
    assert.equal(f.timers.length,0);
    await f.event({type:'completion_intent',agent_id:'peer-2',revision:1,turn_id:'turn-two',pending:false});
    assert.equal(f.timers.length,1);
    await f.timers.shift()();
    assert.equal(f.prompts.length,1);
    await f.call('turn.start',{turnId:'ready-resume',text:f.prompts[0].text});
    assert.equal((await f.step(undefined,'ready-resume'))[0].text,'native');
  }
});

test('shutdown failure has bounded automatic retries and never claims unknown shutdown', async () => {
  const f=await fixture(); await f.launch(); await f.spawn();
  f.host.tool.call=async()=>({deny:'Denied'});
  await f.event({type:'candidate',agent_id:'peer-1'});
  for(let i=0;i<3;i++) { assert.equal(f.timers.length,1); await f.timers.shift()(); }
  assert.equal(f.timers.length,0);
  assert.equal(f.requests.some(r=>r.op==='settle'),false);
  assert.equal(f.store.get('native-run:session-fixture').finished,false);
  const before=f.requests.length;
  for(const text of ['Please investigate.','Use normal Claude.']) {
    const input={text,origin:{kind:'composer'}};
    assert.deepEqual(await f.call('prompt.submit',input),input);
    assert.equal((await f.step(undefined,'normal'))[0].text,'native');
  }
  assert.equal(f.requests.length,before);
});

test('a newer accepted update fences a queued candidate before any worker stop', async () => {
  const f=await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2');
  let release;
  const update=f.call('prompt.submit',{text:'New requirement',origin:{kind:'composer'}},
    input=>new Promise(resolve=>{release=()=>resolve(input);}));
  await setImmediate();
  await f.event({type:'candidate',agent_id:'peer-1'});
  release(); await update;
  await f.event({type:'context',agent_id:'peer-1',revision:2,message:'New requirement'});
  await f.timers.shift()();
  assert.equal(f.requests.some(r=>r.native_tool==='TaskStop'),false);
  assert.equal(f.requests.some(r=>r.op==='settle'),false);
  assert.equal(f.prompts.length,1);
  assert.equal(f.store.get('native-run:session-fixture').phase,'working');
});

test('an update racing closed finalization is rejected once, then ordinary Claude remains usable', async () => {
  const f=await fixture(); await f.launch(); await f.spawn();
  let release;
  const update=f.call('prompt.submit',{text:'Late requirement',origin:{kind:'composer'}},
    input=>new Promise(resolve=>{release=()=>resolve(input);}));
  await setImmediate(); await f.event({type:'candidate',agent_id:'peer-1'});
  await f.timers.shift()(); release();
  assert.match((await update).drop,/not delivered/);
  const input={text:'Now investigate normally.',origin:{kind:'composer'}};
  const count=f.requests.length;
  assert.deepEqual(await f.call('prompt.submit',input),input);
  assert.equal((await f.step(undefined,'normal'))[0].text,'native');
  assert.equal(f.requests.length,count);
});

test('one transient store failure does not poison subsequent durable observations', async () => {
  const f=await fixture(); await f.launch(); await f.spawn();
  const save=f.host.store.set; let calls=0;
  f.host.store.set=async(...args)=>{if(++calls===1)throw new Error('Transient disk failure'); return save(...args);};
  await assert.rejects(f.call('classic.SubagentStop',{agent_id:'peer-1',background_tasks:[]}),/Transient/);
  await f.call('classic.SubagentStop',{agent_id:'peer-1',background_tasks:[]});
  assert.equal(calls,2);
});

test('restored stale bridge and unknown native ownership preserve work without trapping ordinary input', async () => {
  const first=await fixture(); await first.launch(); await first.spawn();
  const reload=await fixture({store:first.store});
  await reload.call('session.start');
  await setImmediate();
  const count=reload.requests.length;
  for(const text of ['Please investigate yourself.','Write the report.']) {
    const input={text,origin:{kind:'composer'}};
    assert.deepEqual(await reload.call('prompt.submit',input),input);
    assert.equal((await reload.step(undefined,'resumed-normal'))[0].text,'native');
  }
  assert.equal(reload.requests.length,count);
  assert.equal(reload.store.get('native-run:session-fixture').finished,false);
});

test('finalized peers cannot execute tools, spawn descendants or resume in removed workspaces', async () => {
  const f=await fixture(); await f.launch(); await f.spawn();
  await f.event({type:'final',status:'complete',delivery:{verification_required:false}});
  assert.match((await f.step('peer-1'))[0].text,/settling or recovering/);
  assert.ok((await f.call('tool.call',{tool:'Bash',agentId:'peer-1',command:'write'})).deny);
  assert.ok((await f.call('agent.spawn',{parentAgentId:'peer-1'})).deny);
  assert.equal((await f.call('session.send',{to:'peer-1',text:'Resume',origin:{kind:'model'}})).isDelivered,false);
  const input={text:'A new task',origin:{kind:'composer'}};
  assert.deepEqual(await f.call('prompt.submit',input),input);
});

test('queued ordinary input is rechecked after a preceding forwarding failure', async () => {
  const f=await fixture(); await f.launch(); await f.spawn();
  let release;
  const process=f.host.process.run;
  f.host.process.run=async(argv,init)=>JSON.parse(init.stdin).op==='update'
    ? new Promise(resolve=>{release=()=>resolve({exitCode:1,stdout:'',stderr:'Bridge unavailable'});}) : process(argv,init);
  const first=f.call('prompt.submit',{text:'Forward me',origin:{kind:'composer'}});
  await setImmediate();
  const input={text:'Use ordinary Claude',origin:{kind:'composer'}};
  const second=f.call('prompt.submit',input);
  release();
  assert.ok((await first).drop);
  assert.deepEqual(await second,input);
});

test('admitted update remains acknowledged when its later control snapshot fails', async () => {
  const f=await fixture(); await f.launch(); await f.spawn();
  const save=f.host.store.set; let failures=1;
  f.host.store.set=async(...args)=>{if(failures-->0)throw new Error('Storage unavailable'); return save(...args);};
  const result=await f.call('prompt.submit',{text:'Accepted requirement',origin:{kind:'composer'}});
  assert.equal(result.drop,undefined);
  assert.equal(f.requests.filter(r=>r.op==='update').length,1);
  assert.match(f.notices.at(-1),/accepted this update.*could not save/);
  const input={text:'Investigate normally',origin:{kind:'composer'}};
  assert.deepEqual(await f.call('prompt.submit',input),input);
});

test('blocked or malformed restoration never blocks session startup or ordinary input', async () => {
  for(const mode of ['pending','malformed','unreadable']) {
    const f=await fixture();
    if(mode==='pending') f.host.store.get=()=>new Promise(()=>{});
    if(mode==='malformed') f.host.store.get=async()=>({session:'wrong-conversation'});
    if(mode==='unreadable') f.host.store.get=async()=>{throw new Error('Storage unavailable');};
    let started=false;
    await f.call('session.start',{},async e=>{started=true;return e;});
    assert.equal(started,true);
    await setImmediate();
    for(const text of ['Hello','Please work normally']) {
      const input={text,origin:{kind:'composer'}};
      assert.deepEqual(await f.call('prompt.submit',input),input);
      assert.equal((await f.step(undefined,'ordinary'))[0].text,'native');
    }
    assert.equal(f.requests.length,0);
  }
});

test('native descendant teammate is stopped by its exact host address instead of its agent id', async () => {
  const f=await fixture(); await f.launch(); await f.spawn();
  await f.call('agent.spawn',{parentAgentId:'peer-1'},async()=>({agentId:'child-id'}));
  f.agents.set('peer-1','completed'); f.agents.set('child-id','running');
  f.host.agent.list=async()=>[...f.agents].map(([id,status])=>({id,status,...(id==='child-id'?{teammateId:'worker@owned-team'}:{})}));
  const stopped=[];
  f.host.tool.call=async input=>{
    stopped.push(input.task_id);
    if(input.task_id==='worker@owned-team') f.agents.set('child-id','killed');
    return {result:{task_id:input.task_id}};
  };
  await f.event({type:'candidate',agent_id:'peer-1'}); await f.timers.shift()();
  assert.deepEqual(stopped,['worker@owned-team']);
  assert.equal(f.requests.some(r=>r.op==='settle'),true);
});

test('retry finishing uses authenticated offline recovery after the original bridge exits', async () => {
  const f=await fixture(); await f.launch(); await f.spawn();
  f.agents.set('peer-1','completed');
  const process=f.host.process.run;
  let dead=false, recovery;
  f.host.process.run=async(argv,init)=>{
    const request=JSON.parse(init.stdin);
    if(argv.includes('recover')) {
      recovery=request;
      return {exitCode:0,stdout:JSON.stringify({type:'final',status:'complete',delivery:{delivered:true}}),stderr:''};
    }
    return dead ? {exitCode:1,stdout:'',stderr:'DeLM is no longer running'} : process(argv,init);
  };
  await f.event({type:'candidate',agent_id:'peer-1'}); dead=true;
  await f.timers.shift()();
  assert.equal(f.store.get('native-run:session-fixture').canRetryFinish,true);
  await f.retryFinish();
  assert.equal(recovery.intent,'deliver');
  assert.equal(recovery.background_tasks_stopped,true);
  assert.equal(recovery.token,ready.token);
  assert.equal(f.store.get('native-run:session-fixture').final.status,'complete');
});

test('explicit stop cannot become delivery when its bridge has exited with a candidate selected', async () => {
  const f=await fixture(); await f.launch(); await f.spawn();
  f.agents.set('peer-1','completed');
  await f.event({type:'candidate',agent_id:'peer-1'});
  let recovery;
  f.host.process.run=async(argv,init)=>{
    if(argv.includes('recover')) {
      recovery=JSON.parse(init.stdin);
      return {exitCode:0,stdout:JSON.stringify({status:'stopped',recovery:{saved:true}}),stderr:''};
    }
    return {exitCode:1,stdout:'',stderr:'DeLM is no longer running'};
  };
  const result=await f.call('command.run',{command:'delm-stop'});
  assert.match(result.text,/stopped and recovered/);
  assert.equal(recovery.intent,'cancel');
  assert.equal(f.store.get('native-run:session-fixture').final.status,'stopped');
});

test('explicit stop on a reloaded conversation cancels before any automatic delivery recovery', async () => {
  const f=await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2');
  await f.event({type:'candidate',agent_id:'peer-1'});
  const reloaded=await fixture({store:f.store});
  reloaded.agents.set('peer-1','completed'); reloaded.agents.set('peer-2','killed');
  const result=await reloaded.call('command.run',{command:'delm-stop'});
  assert.match(result.text,/stopped and recovered/);
  const recoveries=reloaded.requests.filter(request=>request.native_recover);
  assert.equal(recoveries.length,1);
  assert.equal(recoveries[0].intent,'cancel');
  assert.equal(reloaded.store.get('native-run:session-fixture').cancelRequested,true);
});

test('restored explicit cancel intent overrides an older live runtime delivery intent', async () => {
  const f=await fixture(); await f.launch(); await f.spawn('peer-1'); await f.spawn('peer-2');
  await f.event({type:'candidate',agent_id:'peer-1'});
  const stored=f.store.get('native-run:session-fixture');
  stored.cancelRequested=true;
  const reloaded=await fixture({store:f.store,liveBridge:true});
  const nativeRun=reloaded.host.process.run;
  reloaded.host.process.run=async(argv,init)=>{
    if(JSON.parse(init.stdin).op==='status') return {exitCode:0,stdout:JSON.stringify({ok:true,result:{run:{...ready,finalization:stored.finalization}}}),stderr:''};
    return nativeRun(argv,init);
  };
  await reloaded.call('session.start'); await setImmediate();
  assert.equal(reloaded.requests.find(request=>request.op==='cancel').reason,'User requested /delm-stop');
  assert.equal(reloaded.store.get('native-run:session-fixture').finalization.intent,'cancel');
  assert.equal(reloaded.requests.some(request=>request.native_recover && request.intent==='deliver'),false);
});
