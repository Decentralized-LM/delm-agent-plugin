import {
  TOOL_PREFIX, commandOutcome, completeState, decodeLines,
  nativeLaunchContext, parseReply, toolArguments, validateReady,
} from './protocol.js';

let active = null;
let mainTurn = null;
const STORE_PREFIX = 'native-run:';

function snapshot(run) {
  return {
    ready: run.ready, session: run.session, revision: run.revision,
    agents: run.agents, descendants: run.descendants, background: run.background,
    finished: run.finished, final: run.final,
  };
}

async function persist($, run) {
  const value = snapshot(run);
  run.writes = run.writes.then(() => $.store.set(STORE_PREFIX + run.session, value));
  await run.writes;
}

async function request($, run, op, fields = {}) {
  const result = await $.process.run(
    [run.ready.executable, 'claude', 'request', '--socket', run.ready.socket],
    {stdin: JSON.stringify({token: run.ready.token, op, ...fields}) + '\n', timeoutMs: 120000},
  );
  if (result.exitCode !== 0 || result.isStdoutTruncated || result.isStderrTruncated) {
    throw new Error(result.stderr.trim() || 'DeLM native bridge did not complete its request.');
  }
  const reply = parseReply(result.stdout);
  if (Number.isSafeInteger(reply?.revision)) run.revision = Math.max(run.revision, reply.revision);
  if (Number.isSafeInteger(reply?.run?.revision)) run.revision = Math.max(run.revision, reply.run.revision);
  // Actions arrive once on the daemon stream; the RPC copy is not replayed.
  return reply;
}

async function reportFailure($, run, error) {
  run.failure = String(error?.message || error);
  for (const gate of Object.values(run.deliveries || {})) gate.reject(new Error(run.failure));
  run.deliveries = {};
  $.ui.log('DeLM needs attention: ' + run.failure);
  await persist($, run);
}

async function drain($, run, stream, initial) {
  let buffer = initial;
  try {
    for await (const chunk of stream) {
      if (chunk.stream === 'stderr') {
        run.stderr = (run.stderr + chunk.text).slice(-8192);
        continue;
      }
      const decoded = decodeLines(buffer, chunk.text);
      buffer = decoded.rest;
      for (const event of decoded.events) {
        if (event.type === 'action') await actions($, run, event.result?.actions || []);
      }
    }
    if (!run.finished && !run.ending) {
      throw new Error(run.stderr.trim() || 'The native bridge stopped. Use /delm-stop to recover this run.');
    }
  } catch (error) {
    if (!run.finished && !run.ending) await reportFailure($, run, error);
  }
}

async function start($, task) {
  const connection = await $.mcp.connect('delm');
  if (!connection.isConnected) {
    throw new Error('Claude could not connect DeLM\'s native tools: '
      + (connection.message || connection.reason) + '. Reconnect DeLM in /mcp, then retry /delm:run.');
  }
  const session = await $.session.id();
  const project = await $.session.cwd();
  const prompt = await $.fs.read($.plugin.root + '/hooks/worker.md');
  const stream = $.process.spawn({
    argv: [$.plugin.root + '/bin/delm', 'claude', 'serve'],
    input: JSON.stringify({project, session_id: session, task}) + '\n',
  });
  let buffer = '';
  let stderr = '';
  for (;;) {
    const chunk = await stream.next();
    if (chunk.done) throw new Error(stderr.trim() || 'DeLM could not prepare its native workspaces.');
    if (chunk.value.stream === 'stderr') {
      stderr = (stderr + chunk.value.text).slice(-8192);
      continue;
    }
    const decoded = decodeLines(buffer, chunk.value.text);
    buffer = decoded.rest;
    if (!decoded.events.length) continue;
    const ready = validateReady(decoded.events[0]);
    const run = {
      ready, session, prompt, task, revision: ready.revision, agents: {}, descendants: {},
      background: {}, bindings: [], deliveries: {}, updating: null, launches: 0, awaitingLaunch: true,
      launchTurn: null, finished: false, final: null, ending: false,
      writes: Promise.resolve(), stopping: false,
      stream, stderr, failure: null,
    };
    active = run;
    await persist($, run);
    for (const event of decoded.events.slice(1)) {
      if (event.type === 'action') await actions($, run, event.result?.actions || []);
    }
    void drain($, run, stream, buffer);
    return run;
  }
}

async function submitControl($, run, message) {
  if (run.finished && !run.final) return;
  try {
    await $.prompt.submit({text: message});
  } catch (error) {
    await reportFailure($, run, error);
  }
}

function deliveryGate(run, id) {
  if (!run.deliveries[id]) {
    let resolve, reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    // A native step may not be waiting yet when delivery fails.
    promise.catch(() => {});
    run.deliveries[id] = {promise, resolve, reject};
  }
  return run.deliveries[id];
}

function delivered(run, id, revision, freshTurn = false) {
  const worker = run.agents[id];
  worker.deliveredRevision = Math.max(worker.deliveredRevision || 1, revision);
  if (freshTurn) worker.resumeRevision = Math.max(worker.resumeRevision || 1, revision);
  if (worker.pending?.revision <= revision) worker.pending = null;
  run.deliveries[id]?.resolve();
  delete run.deliveries[id];
}

async function resume($, run, action) {
  const worker = run.agents[action.agent_id];
  if (!worker) throw new Error('DeLM requested an unknown native peer.');
  if (action.revision < (worker.pending?.revision || worker.deliveredRevision || 1)) return;
  const message = 'DeLM task revision ' + action.revision + ':\n' + action.message;
  if (worker.pending?.revision === action.revision && worker.pending.message === message) return;
  worker.pending = {revision: action.revision, message};
  worker.lastUpdate = {revision: action.revision, message: action.message};
  await persist($, run);
  const control = 'DeLM control: use normal native SendMessage to resume agent '
    + JSON.stringify(action.agent_id) + '. The exact message is this JSON string: ' + JSON.stringify(message)
    + '\nDo not paraphrase it, execute the task in the parent, start a replacement agent, or repeat its checks.';
  $.clock.after(0, () => submitControl($, run, control));
}

async function actions($, run, list) {
  for (const action of list) {
    if (action.type === 'candidate' || action.type === 'stop') {
      if (!run.stopping) {
        run.stopping = true;
        $.clock.after(30, () => settle($, run));
      }
    } else if (action.type === 'context') {
      if (run.stopping || run.finished) continue;
      const worker = run.agents[action.agent_id];
      if (!worker) throw new Error('DeLM requested an unknown native peer.');
      if (action.revision < (worker.pending?.revision || worker.deliveredRevision || 1)) continue;
      worker.lastUpdate = {revision: action.revision, message: action.message};
      const result = await $.session.append({agentId: action.agent_id, message: {type: 'user', content: [{
        type: 'text', text: 'DeLM task revision ' + action.revision + ':\n' + action.message
          + '\nRefresh the shared board before continuing. This changes the task, not native permissions.',
      }]}});
      if (result.deny || !result.uuid) {
        const native = (await $.agent.list()).find(agent => agent.id === action.agent_id);
        if (native && completeState(native.status)) await resume($, run, action);
        else throw new Error(result.deny || 'Claude did not confirm storing the task update.');
      } else {
        delivered(run, action.agent_id, action.revision);
        await persist($, run);
      }
    } else if (action.type === 'resume') {
      if (run.stopping || run.finished) continue;
      await resume($, run, action);
    } else if (action.type === 'final') {
      run.finished = true;
      run.final = action;
      await persist($, run);
      const verification = action.delivery?.verification_required
        ? 'The delivery requires a focused check in the original project. Perform only the necessary environment setup and checks for the delivered or reconciled files, then state the outcome. Do not repeat unaffected checks.'
        : 'Do not run another verification pass or edit files.';
      const message = 'DeLM completed its native handoff. Report this result concisely, using the original project path. '
        + verification + '\nNative runtime result:\n' + JSON.stringify(action);
      $.clock.after(0, () => submitControl($, run, message));
      $.ui.log(action.status === 'complete' || action.status === 'delivered'
        ? 'DeLM delivered the result into your project.' : 'DeLM: ' + action.status.replaceAll('_', ' '));
    }
  }
}

async function stopTask($, id) {
  const result = await $.tool.call({tool: 'TaskStop', task_id: id});
  return !result.isError && !result.deny && result.result?.task_id === id;
}

async function stopOwned($, run) {
  let native = await $.agent.list();
  for (const [id, worker] of Object.entries({...run.agents, ...run.descendants})) {
    const observed = native.find(agent => agent.id === id);
    if (observed && completeState(observed.status)) {
      worker.status = observed.status;
    } else if (observed && await stopTask($, id)) {
      worker.status = 'killed';
    } else if (!completeState(worker.status)) {
      throw new Error('Native shutdown is not confirmed for agent ' + id + '. Its workspace is preserved.');
    }
  }
  // A completed background shell is proved by a later native SubagentStop snapshot;
  // otherwise require TaskStop's structured acknowledgment for that exact task.
  for (const [id, task] of Object.entries(run.background)) {
    if (task.stopped) continue;
    if (!await stopTask($, id)) {
      throw new Error('Native shutdown is not confirmed for background task ' + id + '. Its workspace is preserved.');
    }
    task.stopped = true;
  }
  // TaskStop and turn.complete can precede the native task registry's final
  // transition. Observe that transition rather than treating an acknowledgment
  // as proof that the agent has already stopped.
  for (let attempt = 0; attempt < 40; attempt++) {
    native = await $.agent.list();
    const pending = native.some(agent => owned(run, agent.id) && !completeState(agent.status));
    if (!pending) break;
    await $.clock.sleep(50);
  }
  for (const [id, worker] of Object.entries({...run.agents, ...run.descendants})) {
    const observed = native.find(agent => agent.id === id);
    if (observed && !completeState(observed.status)) {
      throw new Error('Native agent ' + id + ' is still active. Its workspace is preserved.');
    }
    if (observed) worker.status = observed.status;
  }
  await persist($, run);
  return Object.entries(run.agents).map(([id, worker]) => ({id, status: worker.status}));
}

async function settle($, run) {
  try {
    const agents = await stopOwned($, run);
    await request($, run, 'settle', {agents, background_tasks_stopped: true});
  } catch (error) {
    run.stopping = false;
    await reportFailure($, run, error);
  }
}

async function recover($, stored) {
  const run = {
    ...stored, ready: validateReady(stored.ready), writes: Promise.resolve(),
    bindings: [], deliveries: {}, updating: null, launches: 2, awaitingLaunch: false, failure: null, stopping: true,
  };
  active = run;
  const agents = await stopOwned($, run);
  const result = await $.process.run([run.ready.executable, 'claude', 'recover', '--run-id', run.ready.run_id], {
    stdin: JSON.stringify({token: run.ready.token, session_id: run.session, agents, background_tasks_stopped: true}) + '\n',
    timeoutMs: 120000,
  });
  if (result.exitCode !== 0 || result.isStdoutTruncated || result.isStderrTruncated) {
    throw new Error(result.stderr.trim() || 'DeLM needs recovery before another run can start.');
  }
  const recovered = JSON.parse(result.stdout);
  run.finished = true;
  run.final = recovered;
  await persist($, run);
  $.ui.log('DeLM recovered the interrupted run. ' + (recovered.message || 'Saved work remains available in the run record.'));
}

function owned(run, id) {
  return id && (run?.agents[id] || run?.descendants[id]);
}

function* stoppedStep(event, answer) {
  yield {kind: 'text', index: 0, text: answer};
  yield {kind: 'stop', stopReason: 'end_turn', usage: null};
  return {turnId: event.turnId, index: event.index, answer, toolUses: [], stopReason: 'end_turn', usage: null};
}

export function register(on) {
  on('session.start', async ($, e, next) => {
    await $.command.register({name: 'delm-status', description: 'Show the current DeLM run', immediate: true});
    await $.command.register({name: 'delm-stop', description: 'Stop DeLM and preserve unfinished work', immediate: true});
    const session = await $.session.id();
    const stored = await $.store.get(STORE_PREFIX + session);
    if (stored && !stored.finished) {
      try { await recover($, stored); } catch (error) {
        if (active) await reportFailure($, active, error);
        else $.ui.log('DeLM could not restore its native run record: ' + String(error.message || error));
      }
    }
    return next(e);
  });

  on('command.run', {command: 'delm:run'}, async ($, e, next) => {
    if (active && !active.finished) return {text: 'DeLM is already active. Send a follow-up, or use /delm-stop first.'};
    if (!e.args.trim()) return {text: 'Use /delm:run followed by the task you want to complete.'};
    try {
      const run = await start($, e.args.trim());
      return next({...e, args: nativeLaunchContext(run.task, run.prompt)});
    } catch (error) {
      return {text: 'DeLM could not start: ' + String(error.message || error), exitCode: 1};
    }
  });

  on('command.run', {command: 'delm-status'}, async ($) => {
    if (!active) return {text: 'No DeLM run is active in this session.'};
    if (active.finished) return {text: 'DeLM: ' + (active.final?.status || 'finished')};
    try {
      const status = await request($, active, 'status');
      return {text: JSON.stringify({run_id: status.run.run_id, status: status.run.status, board: status.board}, null, 2)};
    } catch (error) { return {text: String(error.message || error)}; }
  });

  on('command.run', {command: 'delm-stop'}, async ($) => {
    if (!active || active.finished) return {text: 'No DeLM run is active in this session.'};
    try {
      active.stopping = true;
      await request($, active, 'cancel', {reason: 'User requested /delm-stop'});
      $.clock.after(0, () => settle($, active));
      return {text: 'Stopping DeLM and preserving unfinished work.'};
    } catch (error) {
      try { await recover($, snapshot(active)); return {text: 'DeLM stopped and recovered the interrupted run.'}; }
      catch (recoveryError) { return {text: 'DeLM requires recovery: ' + String(recoveryError.message || recoveryError)}; }
    }
  });

  on('turn.start', async ($, e, next) => {
    mainTurn = e.turnId;
    if (active?.awaitingLaunch) {
      active.awaitingLaunch = false;
      active.launchTurn = e.turnId;
    }
    return next(e);
  });

  on('agent.spawn', async ($, e, next) => {
    const run = active;
    if (!run || run.finished) return next(e);
    if (owned(run, e.parentAgentId)) {
      const result = await next(e);
      if (result.agentId) {
        run.descendants[result.agentId] = {parent: e.parentAgentId, status: 'running'};
        await persist($, run);
      }
      return result;
    }
    if (e.parentAgentId || mainTurn !== run.launchTurn) return next(e);
    if (run.launches >= 2) return {deny: 'DeLM already started its two native peers. Resume an existing peer if needed.'};
    if (!e.fork || e.subagentType !== 'fork' || e.name || e.isTeammate) {
      return {deny: 'DeLM requires an unnamed native fork to preserve the current conversation and setup.'};
    }
    const slot = ++run.launches;
    let release;
    const gate = new Promise(resolve => { release = resolve; });
    run.bindings.push(gate);
    try {
      const result = await next({...e, cwd: run.ready.workers[slot - 1].cwd, background: true});
      if (!result.agentId) throw new Error(result.deny || 'Claude did not return a native peer identity.');
      run.agents[result.agentId] = {slot, status: 'running', turn: null,
        deliveredRevision: run.ready.revision, resumeRevision: run.ready.revision,
        acknowledgedRevision: 0, pending: null, lastUpdate: null};
      await persist($, run);
      await request($, run, 'bind', {slot, agent_id: result.agentId});
      await request($, run, 'configure_scopes', {
        agent_id: result.agentId, scopes: [{path: run.ready.workers[slot - 1].cwd, access: 'write'}],
      });
      return result;
    } catch (error) {
      await reportFailure($, run, error);
      try { await request($, run, 'cancel', {reason: 'Native peer launch failed'}); } catch { /* Durable recovery retains the workspace. */ }
      return {deny: 'DeLM could not bind the native peer: ' + String(error.message || error)};
    } finally { release(); }
  });

  on('turn.step', async function* ($, e, next) {
    const run = active;
    // Claude captures this request's messages before invoking turn.step hooks.
    // An update stored while this hook waits belongs to a later native request.
    const capturedRevision = run?.agents[e.agentId]?.deliveredRevision ?? run?.ready.revision;
    if (run && !run.finished && e.agentId) {
      await Promise.all(run.bindings);
      const worker = run.agents[e.agentId];
      if (worker) {
        try {
          await run.updating;
          while (worker.deliveredRevision < run.revision) await deliveryGate(run, e.agentId).promise;
          if (worker.deliveredRevision !== capturedRevision
              || (worker.resumeRevision || 1) < worker.deliveredRevision) {
            return yield* stoppedStep(e, 'DeLM is resuming this peer with the updated task context.');
          }
          await request($, run, 'step', {agent_id: e.agentId, turn_id: e.turnId, revision: worker.deliveredRevision});
          worker.acknowledgedRevision = worker.deliveredRevision;
          worker.turn = e.turnId;
          worker.status = 'running';
        } catch (error) {
          await reportFailure($, run, error);
          return yield* stoppedStep(e, 'DeLM paused this peer because its native coordination state is unavailable.');
        }
      }
    }
    return yield* next(e);
  }).catch(async function* ($, e, next) {
    if (active && owned(active, e.agentId)) {
      return yield* stoppedStep(e, 'DeLM paused this peer because native task delivery did not complete.');
    }
    return yield* next(e);
  });

  on('session.send', async ($, e, next) => {
    const run = active;
    const pending = run?.agents[e.to]?.pending;
    if (!run || run.finished || !pending || e.agentId || e.origin.kind !== 'model') return next(e);
    if (e.text !== pending.message) {
      return {isDelivered: false, reason: 'Send the exact DeLM task update supplied for this peer, without paraphrasing.'};
    }
    const result = await next(e);
    if (result.isDelivered) {
      delivered(run, e.to, pending.revision, true);
      await persist($, run);
    }
    return result;
  });

  on('session.receive', async ($, e, next) => {
    const run = active;
    const pending = run?.agents[e.agentId]?.pending;
    const result = await next(e);
    if (run && !run.finished && pending && e.origin.kind === 'coordinator'
        && e.text === pending.message && result.text === pending.message && !result.consumed) {
      delivered(run, e.agentId, pending.revision, true);
      await persist($, run);
    }
    return result;
  });

  on('tool.call', async ($, e, next) => {
    const run = active;
    const worker = run?.agents[e.agentId];
    if (e.tool.startsWith(TOOL_PREFIX)) {
      if (!run || run.finished || !worker?.turn) return {deny: 'Use /delm:run to bind these tools to native DeLM peers.'};
      let ticket;
      try {
        const arguments_ = toolArguments(e);
        const reservation = await request($, run, 'reserve', {
          agent_id: e.agentId, turn_id: worker.turn, call_id: e.tool_use_id,
          tool: e.tool.slice(TOOL_PREFIX.length), arguments: arguments_,
        });
        ticket = reservation.ticket;
        if (typeof ticket !== 'string') throw new Error('Missing native invocation ticket.');
        // next executes Claude's own MCP permissions, approval UI, and server transport.
        return await next({...e, _delm: {socket: run.ready.socket, ticket}});
      } catch (error) {
        return {deny: 'DeLM refused this coordination call: ' + String(error.message || error)};
      } finally {
        if (ticket && !run.finished) {
          try { await request($, run, 'revoke', {ticket}); } catch { /* Tickets are single-use and never confer control authority. */ }
        }
      }
    }
    if (!run || run.finished || !owned(run, e.agentId) || e.tool !== 'Bash') return next(e);
    let nativeResult;
    try {
      if (worker) {
        await request($, run, 'command_start', {
          agent_id: e.agentId, turn_id: worker.turn, call_id: e.tool_use_id,
          command: e.command, cwd: await $.session.cwd(),
        });
      }
      const result = await next(e);
      nativeResult = result;
      const outcome = commandOutcome(result);
      if (outcome.background_task_id) {
        run.background[outcome.background_task_id] = {agent: e.agentId, stopped: false};
        await persist($, run);
      }
      if (worker && outcome.result_ref !== null) {
        await request($, run, 'command_end', {
          agent_id: e.agentId, call_id: e.tool_use_id, ...outcome,
        });
      }
      return result;
    } catch (error) {
      await reportFailure($, run, error);
      if (nativeResult) return nativeResult;
      return {deny: 'DeLM could not record this native command: ' + String(error.message || error)};
    }
  });

  on('classic.SubagentStop', async ($, e, next) => {
    const run = active;
    if (run && owned(run, e.agent_id) && Array.isArray(e.background_tasks)) {
      const running = new Set(e.background_tasks.map(task => task.id));
      for (const [id, task] of Object.entries(run.background)) {
        if (task.agent === e.agent_id && !running.has(id)) task.stopped = true;
      }
      await persist($, run);
    }
    return next(e);
  });

  on('turn.complete', async ($, e, next) => {
    const run = active;
    const worker = run?.agents[e.agentId];
    if (run && !run.finished && !e.agentId && e.turnId === run.launchTurn
        && Object.keys(run.agents).length !== 2) {
      const reason = 'Claude did not complete the required two-peer native launch.';
      await reportFailure($, run, new Error(reason));
      try { await request($, run, 'cancel', {reason}); }
      catch (error) { await reportFailure($, run, error); }
    }
    if (run && !run.finished && owned(run, e.agentId)) {
      owned(run, e.agentId).status = e.isAborted ? 'killed' : e.reason === 'answer' ? 'completed' : 'failed';
      await persist($, run);
      if (worker) {
        try {
          if (worker.turn === e.turnId) {
            await request($, run, 'turn_end', {
              agent_id: e.agentId, turn_id: e.turnId,
              reason: e.reason === 'answer' && !e.isAborted ? 'completed' : e.isAborted ? 'interrupted' : 'failed',
              answer: e.answer,
            });
          }
          worker.turn = null;
          if (e.reason === 'answer' && !e.isAborted && !run.stopping && worker.lastUpdate
              && worker.lastUpdate.revision > worker.acknowledgedRevision) {
            await resume($, run, {agent_id: e.agentId, ...worker.lastUpdate});
          }
        } catch (error) { if (!run.stopping) await reportFailure($, run, error); }
      }
    }
    return next(e);
  });

  on('prompt.submit', async ($, e, next) => {
    const run = active;
    if (!run || run.finished || run.awaitingLaunch || e.origin.kind === 'plugin' || e.text.startsWith('/')) return next(e);
    if (!['composer', 'bridge', 'sdk'].includes(e.origin.kind)) return next(e);
    const previous = run.updating;
    let release;
    run.updating = new Promise(resolve => { release = resolve; });
    try {
      await previous;
      await request($, run, 'update', {text: e.text});
      await persist($, run);
      return next({...e, context: [...(e.context || []),
        'DeLM is forwarding this update to its native peers. Keep this parent a lightweight control channel; do not duplicate their implementation or checks.']});
    } catch (error) {
      await reportFailure($, run, error);
      return next({...e, context: [...(e.context || []), 'DeLM could not deliver this update. Report the error and do not claim the peers received it.']});
    } finally {
      release();
    }
  });

  on('session.end', async ($, e, next) => {
    if (active && !active.finished) {
      active.ending = true;
      await persist($, active);
      // The host allows 1.5 seconds here. Its bridge process exits on unload;
      // durable recovery proves native shutdown before touching workspace files.
    }
    return next(e);
  });
}
