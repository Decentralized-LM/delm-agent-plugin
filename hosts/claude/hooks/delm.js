import {
  TOOL_PREFIX, commandOutcome, completeState, decodeLines,
  followupText, nativeLaunchContext, parseReply, toolArguments, validateReady,
} from './protocol.js';
import {registerBoard, observeBoard} from './board-view.js';

const runs = new Map();
const restores = new Map();
const starts = new Set();
const generations = new Map();
const restoreAttempts = new Set();
const STORE_PREFIX = 'native-run:';
function ownsConversation(run) {
  return Boolean(run && !run.finished && !run.ending && run.phase === 'working'
    && !run.failure && !run.stopping && !run.finalization && !run.cancelRequested);
}
function updateBoard($, run) {
  run.conversationAvailable = !ownsConversation(run);
  try { observeBoard(run, () => retryFinish($, run.session)); } catch { /* Presentation cannot fail a control operation. */ }
}

function snapshot(run) {
  return {
    ready: run.ready, session: run.session, revision: run.revision,
    agents: run.agents, descendants: run.descendants, background: run.background,
    finished: run.finished, final: run.final, ending: Boolean(run.ending), failure: run.failure || null,
    phase: run.phase, finalization: run.finalization, canRetryFinish: Boolean(run.canRetryFinish),
    controls: run.controls || [],
    completionPending: run.completionPending || {},
    cancelRequested: Boolean(run.cancelRequested),
    conversationAvailable: !ownsConversation(run),
  };
}

async function persist($, run) {
  const value = snapshot(run);
  // Preserve write ordering without permanently inheriting a transient failure.
  // Each caller still observes its own write failure before admitting more work.
  run.writes = run.writes.catch(() => {}).then(() => $.store.set(STORE_PREFIX + run.session, value));
  await run.writes;
  updateBoard($, run);
}

async function currentRun($, restoreInterrupted = true) {
  const session = await $.session.id();
  const known = runs.get(session);
  if (!restoreInterrupted) return known || null;
  if (restores.has(session)) return restores.get(session);
  if (known && (!known.ending || known.finished)) return known;
  const restoring = restore($, session).finally(() => {
    if (restores.get(session) === restoring) restores.delete(session);
  });
  restores.set(session, restoring);
  return restoring;
}

function restoreInBackground($) {
  void (async () => {
    const session = await $.session.id();
    if (restoreAttempts.has(session)) return;
    restoreAttempts.add(session);
    try { await currentRun($); }
    catch (error) {
      $.ui.log('DeLM recovery needs attention: ' + String(error.message || error)
        + '. You can continue chatting normally; no unfinished workspace was removed.');
    }
  })().catch(() => {});
}

async function eventRun($, agentId) {
  if (agentId) {
    for (const run of runs.values()) if (owned(run, agentId)) return run;
  }
  return currentRun($, false);
}

async function restore($, session) {
  const stored = await $.store.get(STORE_PREFIX + session);
  if (!stored || await $.session.id() !== session) return null;
  const known = runs.get(session);
  if (known && (!known.ending || known.finished)) return known;
  if (stored.session !== session) throw new Error('DeLM found a run record for another conversation.');
  if (stored.finished) {
    const run = {...stored, writes: Promise.resolve()};
    runs.set(session, run);
    return run;
  }
  try { return await recover($, stored); }
  catch (error) {
    const run = runs.get(session);
    if (run) await reportFailure($, run, error);
    else $.ui.log('DeLM could not restore this conversation: ' + String(error.message || error));
    return run || null;
  }
}

async function current($, run) {
  return !run.ending && runs.get(run.session) === run && await $.session.id() === run.session;
}

async function request($, run, op, fields = {}, timeoutMs = 120000) {
  const result = await $.process.run(
    [run.ready.executable, 'claude', 'request', '--socket', run.ready.socket],
    {stdin: JSON.stringify({token: run.ready.token, op, ...fields}) + '\n', timeoutMs},
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
  run.phase = 'recovery_required';
  run.canRetryFinish = run.finalization?.intent === 'deliver' && !run.finished && !run.cancelRequested;
  for (const gate of Object.values(run.deliveries || {})) gate.reject(new Error(run.failure));
  run.deliveries = {};
  if (await current($, run)) $.ui.log('DeLM needs attention: ' + run.failure);
  try { await persist($, run); }
  catch { updateBoard($, run); }
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

async function start($, task, session) {
  const generation = generations.get(session) || 0;
  if (await $.session.id() !== session) throw new Error('The conversation changed before preparation. Retry /delm:run here.');
  const connection = await $.mcp.connect('delm');
  if (!connection.isConnected) {
    throw new Error('Claude could not connect DeLM\'s native tools: '
      + (connection.message || connection.reason) + '. Reconnect DeLM in /mcp, then retry /delm:run.');
  }
  if (await $.session.id() !== session) throw new Error('The conversation changed before preparation. Retry /delm:run here.');
  const project = await $.session.cwd();
  const prompt = await $.fs.read($.plugin.root + '/hooks/worker.md');
  let hostVersion = null;
  try { hostVersion = (await $.session.version()).version; } catch { /* Optional diagnostic metadata. */ }
  const stream = $.process.spawn({
    argv: [$.plugin.root + '/bin/delm', 'claude', 'serve'],
    input: JSON.stringify({project, session_id: session, task, host_version: hostVersion, package_root: $.plugin.root}) + '\n',
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
    if (ready.session_id !== session) throw new Error('DeLM returned workspaces for another conversation.');
    const run = {
      ready, session, prompt, task, revision: ready.revision, agents: {}, descendants: {},
      background: {}, bindings: [], deliveries: {}, updating: null, launches: 0, awaitingLaunch: true,
      launchTurn: null, finished: false, final: null, ending: false,
      phase: 'working', finalization: null, canRetryFinish: false, settlement: null,
      retryTimer: null, retryCount: 0, proofVersion: 0,
      writes: Promise.resolve(), stopping: false,
      stream, stderr, failure: null,
    };
    runs.set(session, run);
    restores.delete(session);
    await persist($, run);
    if (await $.session.id() !== session || (generations.get(session) || 0) !== generation) {
      run.ending = true;
      await request($, run, 'cancel', {reason: 'Conversation changed during preparation'});
      await persist($, run);
      throw new Error('The conversation changed during preparation. Return to its conversation and use /delm-stop to recover.');
    }
    for (const event of decoded.events.slice(1)) {
      if (event.type === 'action') await actions($, run, event.result?.actions || []);
    }
    void drain($, run, stream, buffer);
    return run;
  }
}

async function submitControl($, run, message, detail = {kind: 'final'}) {
  if (!await current($, run) || (run.finished && !run.final)) return;
  const control = {text: message, ...detail, admitted: false, turn: null};
  run.controls ||= [];
  run.controls.push(control);
  if (run.controls.length > 32) run.controls.splice(0, run.controls.length - 32);
  try {
    await persist($, run);
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
  $.clock.after(0, () => submitResume($, run, action.agent_id, message, control));
}

async function submitResume($, run, agentId, message, control) {
  if (!ownsConversation(run) || run.agents[agentId]?.pending?.message !== message
      || run.agents[agentId]?.pending?.revision !== run.revision) return;
  if (hasPendingCompletion(run)) {
    (run.heldResumes ||= {})[agentId] = {message, control};
    return;
  }
  await submitControl($, run, control, {
    kind: 'resume', agentId, message, revision: run.agents[agentId].pending.revision,
  });
}

function obsoleteControl(run, control) {
  return control?.kind === 'resume' && (!ownsConversation(run)
    || control.revision !== run.revision
    || run.agents[control.agentId]?.pending?.message !== control.message
    || run.agents[control.agentId]?.pending?.revision !== control.revision);
}

function hasPendingCompletion(run) {
  return Object.values(run.completionPending || {}).some(value => value.revision === run.revision);
}

function releaseHeldResumes($, run) {
  if (!ownsConversation(run) || hasPendingCompletion(run)) return;
  const held = run.heldResumes || {};
  run.heldResumes = {};
  for (const [id, value] of Object.entries(held)) {
    $.clock.after(0, () => submitResume($, run, id, value.message, value.control));
  }
}

async function actions($, run, list) {
  for (const action of list) {
    if (!await current($, run)) continue;
    if (action.id && run.lastAction === action.id) continue;
    if (action.type === 'candidate' || action.type === 'stop') {
      const finalization = action.finalization;
      if (!finalization || !Number.isSafeInteger(finalization.generation)
          || !Number.isSafeInteger(finalization.revision)
          || !['deliver', 'cancel'].includes(finalization.intent)) {
        throw new Error('DeLM received an incompatible finalization contract. Preserve this run for recovery.');
      }
      if (finalization.revision < run.revision
          || finalization.generation < (run.finalization?.generation || 0)) continue;
      run.retryTimer?.cancel(); run.retryTimer = null;
      run.finalization = finalization;
      run.stopping = true;
      run.phase = finalization.intent === 'deliver' ? 'finishing' : 'stopping';
      run.canRetryFinish = false;
      run.retryCount = 0;
      await persist($, run);
      scheduleSettle($, run, 30);
    } else if (action.type === 'completion_intent') {
      if (!run.agents[action.agent_id] || action.revision !== run.revision) continue;
      run.completionPending ||= {};
      if (action.pending) {
        run.completionPending[action.agent_id] = {revision: action.revision, turn: action.turn_id};
      } else if (run.completionPending[action.agent_id]?.turn === action.turn_id) {
        delete run.completionPending[action.agent_id];
      }
      await persist($, run);
      if (!action.pending) releaseHeldResumes($, run);
    } else if (action.type === 'context') {
      await run.updating;
      if (run.stopping || run.finished) continue;
      if (!Number.isSafeInteger(action.revision) || action.revision < run.revision) continue;
      run.revision = action.revision;
      const worker = run.agents[action.agent_id];
      if (!worker) throw new Error('DeLM requested an unknown native peer.');
      if (action.revision < (worker.pending?.revision || 1)
          || action.revision <= (worker.deliveredRevision || 1)) continue;
      worker.lastUpdate = {revision: action.revision, message: action.message};
      const message = {type: 'user', content: [{
        type: 'text', text: 'DeLM task revision ' + action.revision + ':\n' + action.message
          + '\nRefresh the shared board before continuing. This changes the task, not native permissions.',
      }]};
      const result = await $.session.append({agentId: action.agent_id, message});
      if (!await current($, run)) throw new Error('The conversation ended before task delivery was confirmed.');
      if (result.deny || !result.uuid) {
        const native = (await $.agent.list()).find(agent => agent.id === action.agent_id);
        if (native && completeState(native.status)) await resume($, run, action);
        else throw new Error(result.deny || 'Claude did not confirm storing the task update.');
      } else {
        if (JSON.stringify(result.message?.content) !== JSON.stringify(message.content)) {
          throw new Error('Claude did not retain the complete task update for peer ' + action.agent_id + '.');
        }
        delivered(run, action.agent_id, action.revision);
        await persist($, run);
      }
    } else if (action.type === 'resume') {
      await run.updating;
      if (run.stopping || run.finished) continue;
      if (!Number.isSafeInteger(action.revision) || action.revision < run.revision) continue;
      run.revision = action.revision;
      await resume($, run, action);
    } else if (action.type === 'final') {
      if (run.finished) continue;
      run.finished = true;
      run.final = action;
      run.phase = action.status;
      run.stopping = false;
      run.canRetryFinish = false;
      run.failure = null;
      run.retryTimer?.cancel(); run.retryTimer = null;
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
    if (action.id) run.lastAction = action.id;
  }
}

async function stopTask($, run, id, address = id) {
  if (!await current($, run)) throw new Error('Return to the owning conversation before stopping its DeLM peers.');
  const result = await $.tool.call({tool: 'TaskStop', task_id: address});
  return !result.isError && !result.deny && [id, address].includes(result.result?.task_id);
}

async function stopOwned($, run) {
  if (!await current($, run)) throw new Error('Return to the owning conversation before recovering its DeLM peers.');
  // Stop known shell children before parents. Stopping an agent first can
  // cascade to its shell, retiring the native task before TaskStop can return
  // that shell's acknowledgment.
  for (const [id, task] of Object.entries(run.background)) {
    if (task.stopped) continue;
    const acknowledged = await stopTask($, run, id);
    if (!acknowledged && !task.stopped) {
      throw new Error('Native shutdown is not confirmed for background task ' + id + '. Its workspace is preserved.');
    }
    task.stopped = true;
  }
  let native = await $.agent.list();
  // Descendant agent loops can themselves own children; stop deepest first.
  const depth = id => {
    let value = 0, parent = run.descendants[id]?.parent;
    const seen = new Set([id]);
    while (parent && !seen.has(parent)) { seen.add(parent); value++; parent = run.descendants[parent]?.parent; }
    return value;
  };
  for (const [id, worker] of Object.entries({...run.agents, ...run.descendants}).sort(([a], [b]) => depth(b) - depth(a))) {
    const observed = native.find(agent => agent.id === id);
    if (observed && completeState(observed.status)) {
      worker.status = observed.status;
    } else if (observed && await stopTask($, run, id, observed.teammateId || id)) {
      worker.status = 'killed';
    } else if (!completeState(worker.status)) {
      throw new Error('Native shutdown is not confirmed for agent ' + id + '. Its workspace is preserved.');
    }
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

function sameFinalization(run, fence) {
  return !run.finished && run.finalization?.generation === fence?.generation
    && run.finalization?.revision === fence?.revision && run.finalization?.intent === fence?.intent;
}

function scheduleSettle($, run, delay = 0) {
  if (!run.finalization || run.finished || run.ending || run.retryTimer) return;
  const fence = {...run.finalization};
  run.retryTimer = $.clock.after(delay, async () => {
    run.retryTimer = null;
    if (sameFinalization(run, fence)) await settle($, run, fence);
  });
}

async function settle($, run, fence = run.finalization) {
  if (!fence || !sameFinalization(run, fence) || !await current($, run)) return;
  if (run.settlement) { await run.settlement; return; }
  const proofVersion = run.proofVersion || 0;
  run.settlement = (async () => {
    try {
      // This RPC is the admission boundary. A newer update that won first
      // invalidates this generation before any native task is stopped.
      const result = await request($, run, 'begin_settle', {generation: fence.generation, revision: fence.revision});
      if (!sameFinalization(run, fence) || !await current($, run)) return;
      if (run.cancelRequested && result.finalization.intent !== 'cancel') {
        const cancelled = await request($, run, 'cancel', {reason: 'User requested /delm-stop'});
        await actions($, run, cancelled.actions || []);
        return;
      }
      run.finalization = result.finalization;
      run.canRetryFinish = false;
      run.failure = null;
      run.phase = fence.intent === 'deliver' ? 'finishing' : 'stopping';
      await persist($, run);
      const agents = await stopOwned($, run);
      if (!sameFinalization(run, fence) || !await current($, run)) return;
      const final = await request($, run, 'settle', {
        generation: fence.generation, revision: fence.revision, agents, background_tasks_stopped: true,
      });
      // Recovery can reconnect to a live bridge whose original stream reader
      // disappeared. Applying final actions is idempotent across both routes.
      if (run.restored) await actions($, run, final.actions || []);
    } catch (error) {
      if (!sameFinalization(run, fence)) return;
      try {
        await request($, run, 'settlement_failed', {generation: fence.generation, revision: fence.revision});
      } catch { /* A missing bridge retains its candidate for authenticated recovery. */ }
      await reportFailure($, run, error);
    }
  })();
  try { await run.settlement; }
  finally {
    run.settlement = null;
    if (!run.finished && run.finalization) {
      if (!sameFinalization(run, fence)) scheduleSettle($, run);
      else if (run.failure && ((run.proofVersion || 0) > proofVersion || (run.retryCount || 0) < 2)) {
        run.retryCount = (run.retryCount || 0) + 1;
        scheduleSettle($, run, 250 * run.retryCount);
      }
    }
  }
}

async function retryFinish($, session) {
  const run = await currentRun($);
  if (!run || run.session !== session || run.finished || !run.canRetryFinish
      || run.cancelRequested || run.finalization?.intent !== 'deliver') throw new Error('No completed result is awaiting delivery in this conversation.');
  run.retryTimer?.cancel(); run.retryTimer = null;
  run.retryCount = 0;
  if (run.settlement) await run.settlement;
  if (!await current($, run) || run.finished || run.finalization?.intent !== 'deliver') return;
  const recovered = await recover($, snapshot(run));
  if (!recovered.finished && recovered.failure) throw new Error(recovered.failure);
}

async function recover($, stored, requestedIntent = null) {
  const run = {
    ...stored, ready: validateReady(stored.ready), writes: Promise.resolve(),
    bindings: [], deliveries: {}, updating: null, launches: 2, awaitingLaunch: false, failure: null, stopping: true, ending: false,
    phase: 'recovery_required', restored: true, settlement: null, retryTimer: null, retryCount: 0, proofVersion: 0,
  };
  run.cancelRequested = Boolean(stored.cancelRequested || requestedIntent === 'cancel');
  runs.set(run.session, run);
  if (run.cancelRequested) {
    try { await persist($, run); }
    catch { /* The authenticated runtime cancellation below is also durable. */ }
  }
  let live;
  try { live = await request($, run, 'status', {}, 2000); } catch { /* An exited bridge uses durable recovery below. */ }
  if (live?.run) {
    run.finalization = live.run.finalization;
    if (live.finished && live.final) { await actions($, run, [live.final]); return run; }
    if (!run.finalization || (run.cancelRequested && run.finalization.intent !== 'cancel')) {
      const stopped = await request($, run, 'cancel', {reason: run.cancelRequested ? 'User requested /delm-stop' : 'Conversation recovery requested'});
      await actions($, run, stopped.actions || []);
    }
    await settle($, run);
    return run;
  }
  const agents = await stopOwned($, run);
  const result = await $.process.run([run.ready.executable, 'claude', 'recover', '--run-id', run.ready.run_id], {
    stdin: JSON.stringify({token: run.ready.token, session_id: run.session, agents, background_tasks_stopped: true,
      intent: run.cancelRequested ? 'cancel' : requestedIntent || run.finalization?.intent || 'auto'}) + '\n',
    timeoutMs: 120000,
  });
  if (result.exitCode !== 0 || result.isStdoutTruncated || result.isStderrTruncated) {
    throw new Error(result.stderr.trim() || 'DeLM needs recovery before another run can start.');
  }
  const recovered = JSON.parse(result.stdout);
  run.finished = true;
  run.final = recovered;
  run.phase = recovered.status;
  run.canRetryFinish = false;
  await persist($, run);
  if (await current($, run)) $.ui.log('DeLM recovered the interrupted run. ' + (recovered.message || 'Saved work remains available in the run record.'));
  return run;
}

function owned(run, id) {
  return id && (run?.agents[id] || run?.descendants[id]);
}

async function waitForOwnBinding(run, agentId) {
  for (;;) {
    const own = run.bindings.find(value => value.agentId === agentId);
    if (own) { await own.promise; return; }
    // A child can enter its first step before agent.spawn returns its identity.
    // Wait only until that identity is known, never for the other peer's setup.
    const pending = run.bindings.filter(value => !value.done);
    if (!pending.length) return;
    await Promise.race(pending.map(value => value.promise));
  }
}

function* stoppedStep(event, answer) {
  yield {kind: 'text', index: 0, text: answer};
  yield {kind: 'stop', stopReason: 'end_turn', usage: null};
  return {turnId: event.turnId, index: event.index, answer, toolUses: [], stopReason: 'end_turn', usage: null};
}

export function register(on) {
  registerBoard(on);
  on('session.start', async ($, e, next) => {
    await $.command.register({name: 'delm-status', description: 'Show the DeLM board', immediate: true});
    await $.command.register({name: 'delm-stop', description: 'Stop DeLM and save unfinished changes', immediate: true});
    restoreInBackground($);
    return next(e);
  });

  on('command.run', {command: 'delm:run'}, async ($, e, next) => {
    const session = await $.session.id();
    if (starts.has(session)) return {text: 'DeLM is preparing this conversation. Wait for its native peers to start.'};
    if (!e.args.trim()) return {text: 'Use /delm:run followed by the task you want to complete.'};
    starts.add(session);
    try {
      const known = await currentRun($);
      if (known && !known.finished) return {text: ownsConversation(known)
        ? 'DeLM is already working in this conversation. Send a follow-up, or use /delm-stop first.'
        : 'The previous DeLM run needs finishing or recovery before another run can start. You can use Claude normally here. Open /delm-status for its recovery state, or use /delm-stop to save unfinished changes.'};
      const run = await start($, e.args.trim(), session);
      return next({...e, args: nativeLaunchContext(run.task, run.prompt)});
    } catch (error) {
      const run = runs.get(session);
      if (run && !run.finished) await reportFailure($, run, error);
      return {text: 'DeLM could not start: ' + String(error.message || error), exitCode: 1};
    } finally { starts.delete(session); }
  });

  on('command.run', {command: 'delm-stop'}, async ($) => {
    let active;
    try {
      active = await currentRun($, false);
      if (!active) {
        const session = await $.session.id();
        const stored = await $.store.get(STORE_PREFIX + session);
        if (stored && stored.session !== session) throw new Error('DeLM found a run record for another conversation.');
        active = await currentRun($, false);
        if (!active && stored && !stored.finished) {
          const recovered = await recover($, {...stored, cancelRequested: true}, 'cancel');
          return {text: recovered.finished ? 'DeLM stopped and recovered the interrupted run.'
            : 'DeLM could not yet confirm shutdown. Its work is preserved, and you can continue chatting normally.'};
        }
      }
    }
    catch (error) { return {text: 'DeLM recovery needs attention: ' + String(error.message || error) + '. You can continue chatting normally.'}; }
    if (!active || active.finished) return {text: 'No DeLM run is active in this session.'};
    try {
      active.cancelRequested = true;
      active.stopping = true;
      active.phase = 'stopping';
      active.canRetryFinish = false;
      active.retryTimer?.cancel(); active.retryTimer = null;
      if (active.finalization) active.finalization = {...active.finalization, intent:'cancel'};
      updateBoard($, active);
      try { await persist($, active); }
      catch { /* Still record the explicit stop in the authoritative runtime. */ }
      const result = await request($, active, 'cancel', {reason: 'User requested /delm-stop'});
      await actions($, active, result.actions || []);
      scheduleSettle($, active);
      return {text: 'Stopping DeLM and preserving unfinished work.'};
    } catch (error) {
      try {
        const recovered = await recover($, snapshot(active), 'cancel');
        return {text: recovered.finished ? 'DeLM stopped and recovered the interrupted run.'
          : 'DeLM could not yet confirm shutdown. Its work is preserved, and you can continue chatting normally.'};
      }
      catch (recoveryError) { return {text: 'DeLM requires recovery: ' + String(recoveryError.message || recoveryError)}; }
    }
  });

  on('turn.start', async ($, e, next) => {
    const active = await currentRun($, false);
    if (active) active.mainTurn = e.turnId;
    const control = active?.controls?.find(value => value.admitted && !value.turn && value.text === e.text);
    if (control) control.turn = e.turnId;
    if (active?.awaitingLaunch) {
      active.awaitingLaunch = false;
      active.launchTurn = e.turnId;
    }
    return next(e);
  });

  on('agent.spawn', async ($, e, next) => {
    const run = await eventRun($, e.parentAgentId);
    if (run && ((!run.finished && !await current($, run))
        || (owned(run, e.parentAgentId) && !ownsConversation(run)))) {
      return {deny: 'DeLM cannot launch work while its conversation is ending or recovering.'};
    }
    if (!run || run.finished) return next(e);
    if (owned(run, e.parentAgentId)) {
      const result = await next(e);
      if (result.agentId) {
        run.descendants[result.agentId] = {parent: e.parentAgentId, status: 'running'};
        await persist($, run);
      }
      return result;
    }
    if (e.parentAgentId || run.mainTurn !== run.launchTurn) return next(e);
    if (run.launches >= 2) return {deny: 'DeLM already started its two native peers. Resume an existing peer if needed.'};
    if (!e.fork || e.subagentType !== 'fork' || e.name || e.isTeammate) {
      return {deny: 'DeLM requires an unnamed native fork to preserve the current conversation and setup.'};
    }
    const slot = ++run.launches;
    let release;
    const gate = new Promise(resolve => { release = resolve; });
    const binding = {promise: gate, agentId: null, done: false};
    run.bindings.push(binding);
    try {
      const result = await next({...e, cwd: run.ready.workers[slot - 1].cwd, background: true});
      if (!result.agentId) throw new Error(result.deny || 'Claude did not return a native peer identity.');
      binding.agentId = result.agentId;
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
    } finally { binding.done = true; release(); }
  });

  on('turn.step', async function* ($, e, next) {
    // Capture before any API await: Claude already captured this model request.
    const captured = [...runs.values()].find(value => owned(value, e.agentId));
    const capturedRevision = captured?.agents[e.agentId]?.deliveredRevision ?? 1;
    const run = await eventRun($, e.agentId);
    if (run && !e.agentId) {
      const control = run.controls?.find(value => value.turn === e.turnId);
      if (obsoleteControl(run, control)) return yield* stoppedStep(e, '');
      if (control?.kind === 'resume' && hasPendingCompletion(run)) {
        (run.heldResumes ||= {})[control.agentId] = {message: control.message, control: control.text};
        return yield* stoppedStep(e, '');
      }
    }
    if (run && !run.finished && !await current($, run)) {
      return yield* stoppedStep(e, 'DeLM stopped this peer because its conversation ended.');
    }
    if (run && owned(run, e.agentId) && !ownsConversation(run)) {
      return yield* stoppedStep(e, 'DeLM stopped this peer while its run is settling or recovering.');
    }
    if (run && owned(run, e.agentId) && run.failure) {
      return yield* stoppedStep(e, 'DeLM paused this peer: ' + run.failure);
    }
    if (ownsConversation(run) && !e.agentId) {
      await run.updating;
    }
    // Claude captures this request's messages before invoking turn.step hooks.
    // An update stored while this hook waits belongs to a later native request.
    if (run && !run.finished && e.agentId) {
      await waitForOwnBinding(run, e.agentId);
      if (owned(run, e.agentId) && !ownsConversation(run)) {
        return yield* stoppedStep(e, 'DeLM stopped this peer while its run is settling or recovering.');
      }
      const worker = run.agents[e.agentId];
      if (worker) {
        try {
          await run.updating;
          while (worker.deliveredRevision < run.revision) await deliveryGate(run, e.agentId).promise;
          if (worker.deliveredRevision !== capturedRevision
              || (worker.resumeRevision || 1) < worker.deliveredRevision) {
            return yield* stoppedStep(e, 'DeLM is resuming this peer with the updated task context.');
          }
          if (!await current($, run)) throw new Error('The conversation ended before this peer could continue.');
          await request($, run, 'step', {agent_id: e.agentId, turn_id: e.turnId, revision: worker.deliveredRevision});
          if (!await current($, run)) throw new Error('The conversation changed during native step admission.');
          worker.acknowledgedRevision = worker.deliveredRevision;
          worker.turn = e.turnId;
          worker.status = 'running';
          updateBoard($, run);
        } catch (error) {
          await reportFailure($, run, error);
          return yield* stoppedStep(e, 'DeLM paused this peer because its native coordination state is unavailable.');
        }
      }
    }
    return yield* next(e);
  }).catch(async function* ($, e, next) {
    const active = await eventRun($, e.agentId);
    if (active && owned(active, e.agentId)) {
      return yield* stoppedStep(e, 'DeLM paused this peer because native task delivery did not complete.');
    }
    return yield* next(e);
  });

  on('session.send', async ($, e, next) => {
    const run = await eventRun($, e.to);
    if (run && !run.finished && !await current($, run)) return {isDelivered: false, reason: 'This DeLM peer belongs to another or ended conversation.'};
    const pending = run?.agents[e.to]?.pending;
    if (run && owned(run, e.to) && !ownsConversation(run)) return {isDelivered: false, reason: 'This DeLM run is not accepting worker updates. Finish or recover it through its controls.'};
    if (!run || run.finished || !pending || e.agentId || e.origin.kind !== 'model') return next(e);
    if (e.text !== pending.message) {
      return {isDelivered: false, reason: 'Send the exact DeLM task update supplied for this peer, without paraphrasing.'};
    }
    const result = await next(e);
    if (result.isDelivered && await current($, run)) {
      delivered(run, e.to, pending.revision, true);
      await persist($, run);
    }
    return result;
  });

  on('session.receive', async ($, e, next) => {
    const run = await eventRun($, e.agentId);
    if (run && owned(run, e.agentId) && !ownsConversation(run)) return {consumed: 'This DeLM run is not accepting worker updates.'};
    if (run && !run.finished && !await current($, run)) return {consumed: 'This DeLM conversation has ended.'};
    const pending = run?.agents[e.agentId]?.pending;
    const result = await next(e);
    if (run && !run.finished && pending && await current($, run) && e.origin.kind === 'coordinator'
        && e.text === pending.message && result.text === pending.message && !result.consumed) {
      delivered(run, e.agentId, pending.revision, true);
      await persist($, run);
    }
    return result;
  });

  on('tool.call', async ($, e, next) => {
    const run = await eventRun($, e.agentId);
    if (run && owned(run, e.agentId) && (!await current($, run) || !ownsConversation(run))) {
      return {deny: 'This DeLM conversation has ended. Return to it and use /delm-stop to recover.'};
    }
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
        run.background[outcome.background_task_id] = {agent: e.agentId, toolUseId: e.tool_use_id, stopped: false};
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
    const run = await eventRun($, e.agent_id);
    if (run && owned(run, e.agent_id) && Array.isArray(e.background_tasks)) {
      let changed = false;
      const running = new Set(e.background_tasks.map(task => task.id));
      for (const [id, task] of Object.entries(run.background)) {
        if (task.agent === e.agent_id && !running.has(id) && !task.stopped) {
          task.stopped = true; changed = true;
        }
      }
      await persist($, run);
      if (changed) {
        run.proofVersion = (run.proofVersion || 0) + 1;
        scheduleSettle($, run);
      }
    }
    return next(e);
  });

  on('ui.render', {component: 'UserMessage'}, async ($, e, next) => {
    // Native read-only notification fields, independent of the DeLM board.
    // Text, a missing task, and another plugin's message prove nothing.
    const notification = e.props?.task;
    if (e.props?.origin?.kind === 'task-notification' && notification?.id
        && ['completed', 'failed', 'killed'].includes(notification.status)) {
      const run = await currentRun($, false);
      const task = run?.background?.[notification.id];
      if (task && !task.stopped && (!notification.toolUseId || notification.toolUseId === task.toolUseId)) {
        task.stopped = true;
        task.proof = {source: 'native-task-notification', status: notification.status};
        run.proofVersion = (run.proofVersion || 0) + 1;
        $.clock.after(0, async () => {
          try { await persist($, run); scheduleSettle($, run); }
          catch (error) { await reportFailure($, run, error); }
        });
      }
    }
    return next(e);
  });

  on('turn.complete', async ($, e, next) => {
    const run = await eventRun($, e.agentId);
    if (run && !await current($, run)) return next(e);
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
          updateBoard($, run);
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
    const run = await currentRun($, false);
    if (!run) restoreInBackground($);
    if (run && e.origin.kind === 'plugin' && e.origin.name === $.plugin.name) {
      const control = run.controls?.find(value => !value.admitted && value.text === e.text);
      if (control) {
        if (obsoleteControl(run, control)) return {drop: 'DeLM no longer needs this worker control.'};
        if (control.kind === 'resume' && hasPendingCompletion(run)) {
          (run.heldResumes ||= {})[control.agentId] = {message: control.message, control: control.text};
          // Retire this admission so a later event-driven retry has its own
          // record; no parent model call is needed while completion is pending.
          control.admitted = true;
          control.turn = 'held';
          return {drop: 'DeLM is waiting for a native completion already in progress.'};
        }
        control.admitted = true;
        await persist($, run);
        return next(e);
      }
    }
    // The run owns execution resources until cleanup, but owns conversation
    // input only while actively working. Failed/restored runs must not trap
    // ordinary Claude prompts behind a dead bridge or a stop request.
    if (!ownsConversation(run) || e.origin.kind === 'plugin' || e.text.startsWith('/')) return next(e);
    if (!['composer', 'bridge', 'sdk'].includes(e.origin.kind)) return next(e);
    try { followupText(e); }
    catch (error) { return {drop: String(error.message || error)}; }
    const previous = run.updating;
    let release;
    run.updating = new Promise(resolve => { release = resolve; });
    const control = 'DeLM is forwarding this update to its native peers. Keep this parent a lightweight control channel; do not duplicate their implementation or checks.';
    let accepted;
    let admitted = false;
    try {
      await previous;
      if (!ownsConversation(run)) return next(e);
      if (!await current($, run)) return {drop: 'The conversation changed before DeLM could forward this update.'};
      // Native and installed prompt middleware may refuse or rewrite input.
      // Forward its accepted text and additional context, never a discarded prompt.
      accepted = await next({...e, context: [...(e.context || []), control]});
      if (accepted.drop) return accepted;
      if (!await current($, run)) throw new Error('The conversation changed before this update reached DeLM.');
      const text = followupText({...e, ...accepted, context: (accepted.context || []).filter(value => value !== control)});
      await request($, run, 'update', {text});
      admitted = true;
      // An accepted revision won before begin_settle closed admission. Fence
      // out the old timer and allow this revision's delivery actions through.
      if (run.finalization && run.finalization.revision < run.revision) {
        run.retryTimer?.cancel(); run.retryTimer = null;
        run.finalization = null;
        run.stopping = false;
      }
      run.phase = 'working';
      run.failure = null;
      run.completionPending = {};
      await persist($, run);
      releaseHeldResumes($, run);
      return accepted;
    } catch (error) {
      await reportFailure($, run, admitted
        ? new Error('DeLM accepted this update, but could not save its control state: ' + String(error.message || error)) : error);
      if (admitted) return accepted;
      return {drop: 'DeLM could not confirm delivery of this update. ' + String(error.message || error)
        + ' It will not be implemented in the parent or replayed automatically. You can continue chatting normally here; use the DeLM board to finish or recover the run.'};
    } finally {
      release();
    }
  });

  on('session.end', async ($, e, next) => {
    const session = e.sessionId || await $.session.id();
    restoreAttempts.delete(session);
    generations.set(session, (generations.get(session) || 0) + 1);
    const run = runs.get(session);
    restores.delete(session);
    if (run && !run.finished) {
      run.ending = true;
      run.mainTurn = null;
      for (const gate of Object.values(run.deliveries)) gate.reject(new Error('Conversation ended before task delivery.'));
      run.deliveries = {};
      await persist($, run);
      // The host's total session.end budget is 1.5 seconds. Cancel admission
      // promptly; confirm native shutdown through recovery when returning.
      try { await request($, run, run.finalization?.intent === 'deliver' ? 'interrupt' : 'cancel', {reason: 'Conversation ended: ' + e.reason}, 750); }
      catch { /* Durable recovery keeps unfinished work and verifies shutdown. */ }
    }
    return next(e);
  });
}
