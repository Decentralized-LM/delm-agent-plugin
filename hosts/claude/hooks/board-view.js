import {renderBoard, renderCompact, boardSummary, cleanText} from './board-render.js';

const PANE = 'delm';
const STORE = 'native-board:';
const MAX_LINE = 256 * 1024;
const COLLECTIONS = new Set(['tasks', 'shared', 'checks']);
const observedRuns = new Map();
const retryFinishActions = new Map();
const preferenceWrites = new Map();

export function observeBoard(run, retryFinish) {
  // An explicit action closure stays in the host module's native API scope.
  // Passing the native $ context through a dynamic callback is not supported.
  if (retryFinish) retryFinishActions.set(run.session, retryFinish);
  // Copy only presentation facts, never the transport credentials or worker messages.
  observedRuns.set(run.session, {
    session: run.session, ready: {run_id: run.ready?.run_id}, revision: run.revision,
    agents: Object.fromEntries(nativeAgents(run).map(agent => [agent.id, agent])),
    finished: run.finished, ending: run.ending, stopping: run.stopping, failure: run.failure,
    phase: run.phase, canRetryFinish: Boolean(run.canRetryFinish), conversationAvailable: Boolean(run.conversationAvailable),
    final: run.final ? {status: run.final.status, delivery: run.final.delivery} : null,
  });
}

function nativeAgents(run) {
  return Object.entries(run?.agents || {}).map(([id, agent]) => ({
    id, slot: agent.slot, status: agent.status, turn: agent.turn,
    deliveredRevision: agent.deliveredRevision, acknowledgedRevision: agent.acknowledgedRevision,
  }));
}

function initialSnapshot(run, session) {
  const final = run?.final || {};
  const delivery = final.delivery || {};
  const artifacts = Array.isArray(delivery.artifacts) ? delivery.artifacts : [];
  const undelivered = Array.isArray(delivery.undelivered_outputs) ? delivery.undelivered_outputs : [];
  return {
    schema_version: 1, type: 'view', session_id: session, run_id: run?.ready?.run_id || null,
    revision: run?.revision || 1, status: run?.failure ? 'recovery_required'
      : final.status || run?.phase || (run?.ending ? 'interrupted' : run?.stopping ? 'stopping' : 'prepared'),
    finished: Boolean(run?.finished), agents: nativeAgents(run).map(agent => ({
      slot: agent.slot, native_agent_id: agent.id,
      native_state: agent.turn && agent.status === 'running' ? 'working'
        : ['completed', 'killed', 'failed'].includes(agent.status) ? 'stopped' : 'starting',
      task_ids: [],
    })),
    tasks: {items: [], total: 0}, shared: {items: [], total: 0}, checks: {items: [], total: 0},
    outcome: {
      delivered: delivery.delivered ?? null,
      verification_required: delivery.verification_required ?? null,
      cleanup_complete: delivery.cleanup_complete ?? null,
      reason: run?.failure ? cleanText(run.failure) : null,
      artifacts: artifacts.slice(0, 16).map(path => cleanText(path, 512)),
      artifacts_total: artifacts.length,
      artifacts_declared: delivery.artifacts_declared === true,
      undelivered_outputs: undelivered.slice(0, 16).map(path => cleanText(path, 512)),
      undelivered_outputs_total: undelivered.length,
    }, freshness: {unavailable: ['board']},
  };
}

export function validateSnapshot(value, session, runId) {
  if (value?.type !== 'view' || value.schema_version !== 1 || value.session_id !== session
      || value.run_id !== runId || !Number.isSafeInteger(value.revision) || value.revision < 1
      || typeof value.status !== 'string' || typeof value.finished !== 'boolean') {
    throw new Error('The DeLM board received an incompatible snapshot.');
  }
  for (const name of COLLECTIONS) {
    const group = value[name];
    if (!group || !Array.isArray(group.items) || group.items.length > 32
        || !Number.isSafeInteger(group.total) || group.total < group.items.length) {
      throw new Error('The DeLM board received an invalid collection.');
    }
  }
  if (!Array.isArray(value.agents) || value.agents.length > 32 || !value.source
      || !['controller_sequence', 'board_sequence'].every(key => Number.isSafeInteger(value.source[key]) && value.source[key] >= 0)) {
    throw new Error('The DeLM board received invalid source state.');
  }
  return value;
}

export function acceptsSnapshot(previous, incoming) {
  if (!previous?.source) return true;
  return incoming.revision >= previous.revision
    && incoming.source.controller_sequence >= previous.source.controller_sequence
    && incoming.source.board_sequence >= previous.source.board_sequence;
}

// Observation never mutates lifecycle. The explicit retry control delegates to
// the host, which revalidates the current session and delivery intent.
let active = null;
let generation = 0;
let lookupOverride;
let retryFinishAction;
function canRetry(session) { return retryFinishActions.has(session) || Boolean(retryFinishAction); }
function retryFinishing(session) {
  const action = retryFinishActions.get(session);
  return action ? action() : retryFinishAction?.(session);
}

async function readRun($, session) {
  const value = lookupOverride ? await lookupOverride(null, session)
    : observedRuns.get(session) || await $.store.get('native-run:' + session);
  return value?.session === session ? value : null;
}

const valid = state => active === state && state.generation === generation && !state.disposed;
function cancel(timer) { try { timer?.cancel?.(); } catch {} }
function ignore(promise) { Promise.resolve(promise).catch(() => {}); }
function invalidate($, state) {
  if (!valid(state) || state.paint) return;
  state.paint = $.clock.after(250, () => {
    state.paint = null;
    if (valid(state)) { try { $.ui.invalidate('ui.render'); } catch {} }
  });
}
function remember($, state) {
  const value = {version: 1, runId: state.runId, hidden: state.hidden,
    finalSnapshot: state.snapshot?.finished ? state.snapshot : null};
  // Run replacement must not let an older delayed write erase the new preference.
  const writing = (preferenceWrites.get(state.session) || Promise.resolve())
    .then(() => $.store.set(STORE + state.session, value)).catch(() => {});
  state.writes = writing;
  preferenceWrites.set(state.session, writing);
  writing.then(() => {
    if (preferenceWrites.get(state.session) === writing) preferenceWrites.delete(state.session);
  });
}
function endStream(state) {
  state.streamGeneration++;
  const stream = state.stream;
  state.stream = null;
  if (stream?.return) ignore(stream.return());
  cancel(state.retry); state.retry = null;
}
function dispose(state) {
  if (!state) return;
  state.disposed = true;
  endStream(state); cancel(state.paint); cancel(state.paneTimer); cancel(state.nativeTimer);
  cancel(state.navigationTimer);
  state.detailGeneration++;
}
function replace(session, run) {
  dispose(active);
  const state = {
    session, runId: run?.ready?.run_id || null, generation: ++generation,
    snapshot: run ? initialSnapshot(run, session) : null,
    nativeAgents: nativeAgents(run), nativeRevision: 0,
    canRetryFinish: Boolean(canRetry(session) && run?.canRetryFinish), conversationAvailable: Boolean(run?.conversationAvailable),
    hostFailure: run?.failure ? cleanText(run.failure, 600) : null,
    screen: {kind: 'overview'}, pages: {}, pagePrevious: {}, history: [], hidden: false, paneShown: false,
    disconnected: false, attention: null, phase: run ? null : 'preparing',
    streamGeneration: 0, detailGeneration: 0, retries: 0, writes: Promise.resolve(),
  };
  active = state;
  return state;
}
function problem($, state, error) {
  if (!valid(state)) return;
  state.disconnected = true;
  state.attention = 'Board updates unavailable. Use /delm-status to retry.';
  state.viewError = cleanText(error?.message || error, 600);
  invalidate($, state);
}
function syncPanes($, state) {
  if (!valid(state) || state.paneQuery) return;
  state.paneQuery = true;
  Promise.resolve($.ui.panes()).then(panes => {
    if (!valid(state)) return;
    const pane = panes.find(item => item.id === PANE);
    const shown = Boolean(pane?.isPlaced && pane.isShown);
    if (state.paneShown !== shown) { state.paneShown = shown; invalidate($, state); }
  }).catch(() => {}).finally(() => { state.paneQuery = false; });
}
function startPaneSync($, state) {
  if (!valid(state) || state.paneTimer || !state.uiReady || state.snapshot?.finished) return;
  syncPanes($, state);
  state.paneTimer = $.clock.every(1000, () => syncPanes($, state));
}
function open($, state) {
  if (!valid(state)) return Promise.resolve(false);
  state.hidden = false;
  remember($, state);
  // Called directly from the user's command or button, preserving native placement.
  let opened;
  try { opened = $.ui.open({id: PANE, title: 'DeLM', rows: 24, columns: 54}); }
  catch (error) { problem($, state, error); return Promise.resolve(false); }
  return Promise.resolve(opened).then(result => {
    if (!valid(state)) return false;
    state.uiReady = true;
    state.paneShown = Boolean(result.isPlaced);
    startPaneSync($, state);
    // Preparation may finish while the native pane is still opening.
    if (state.snapshot?.finished && !state.snapshot.source) ignore(readFinal($, state));
    else watch($, state);
    invalidate($, state);
    return result.isPlaced;
  }).catch(error => { problem($, state, error); return false; });
}
function argv($, state, extra = []) {
  return [$.plugin.root + '/bin/delm', 'claude', 'view', '--run-id', state.runId,
    '--session-id', state.session, ...extra];
}
async function readOnce($, state, extra = []) {
  const result = await $.process.run(argv($, state, extra), {stdin: '', timeoutMs: 3000});
  if (result.exitCode !== 0 || result.isStdoutTruncated || result.stdout.length > MAX_LINE) {
    throw new Error('DeLM could not read this saved board.');
  }
  return validateSnapshot(JSON.parse(result.stdout), state.session, state.runId);
}
function readFinal($, state) {
  if (state.finalRead) return state.finalRead;
  state.finalRead = readOnce($, state).then(value => accept($, state, value)).catch(error => {
    if (!state.snapshot?.source) problem($, state, error);
  }).finally(() => { state.finalRead = null; });
  return state.finalRead;
}
function accept($, state, value) {
  if (!valid(state) || !acceptsSnapshot(state.snapshot, value)) return false;
  state.snapshot = value;
  state.phase = !value.finished && state.hostFailure ? 'recovery_required' : null;
  state.disconnected = false;
  state.attention = (!value.finished && state.hostFailure) || value.outcome?.reason || null;
  state.retries = 0;
  if (value.finished) {
    remember($, state);
    cancel(state.retry); state.retry = null;
    cancel(state.paneTimer); state.paneTimer = null;
    cancel(state.nativeTimer); state.nativeTimer = null;
  }
  invalidate($, state);
  return true;
}
function watch($, state) {
  if (!valid(state) || !state.runId || state.stream || !state.uiReady || state.snapshot?.finished) return;
  const stamp = ++state.streamGeneration;
  const interval = state.hidden ? 1000 : 250;
  void (async () => {
    let buffer = '';
    try {
      const stream = $.process.spawn({argv: argv($, state, ['--watch', '--interval-ms', String(interval)])});
      state.stream = stream;
      watchRead: for await (const chunk of stream) {
        if (!valid(state) || state.streamGeneration !== stamp) break;
        if (chunk.stream !== 'stdout') continue;
        buffer += chunk.text;
        if (buffer.length > MAX_LINE * 2) throw new Error('DeLM board output exceeded its limit.');
        let index;
        while ((index = buffer.indexOf('\n')) !== -1) {
          const line = buffer.slice(0, index); buffer = buffer.slice(index + 1);
          if (line.length > MAX_LINE) throw new Error('DeLM board record exceeded its limit.');
          if (!line.trim()) continue;
          const value = validateSnapshot(JSON.parse(line), state.session, state.runId);
          if (accept($, state, value) && value.finished) break watchRead;
        }
      }
      if (valid(state) && state.streamGeneration === stamp && !state.snapshot?.finished) {
        throw new Error('The DeLM board observer disconnected.');
      }
    } catch (error) {
      if (valid(state) && state.streamGeneration === stamp) {
        problem($, state, error);
        if (state.retries < 3) {
          const wait = [1000, 2500, 5000][state.retries++];
          state.retry = $.clock.after(wait, () => { state.retry = null; watch($, state); });
        }
      }
    } finally {
      if (state.streamGeneration === stamp) state.stream = null;
    }
  })();
}
function restart($, state) {
  endStream(state);
  watch($, state);
}
function observe($, run) {
  const state = active;
  if (!state || !valid(state) || state.session !== run.session) return;
  if (state.runId && state.runId !== run.ready?.run_id) return;
  const previousRunId = state.runId;
  state.runId = run.ready?.run_id || state.runId;
  state.nativeAgents = nativeAgents(run); state.nativeRevision++;
  if (!state.snapshot) state.snapshot = initialSnapshot(run, state.session);
  state.canRetryFinish = Boolean(canRetry(state.session) && run.canRetryFinish && !run.finished);
  state.conversationAvailable = Boolean(run.conversationAvailable);
  state.hostFailure = run.failure ? cleanText(run.failure, 600) : null;
  // Host observations cannot leave a stale stopping phase over a new durable
  // revision. A current host error is still an independent attention fact.
  state.phase = !state.snapshot?.finished && state.hostFailure ? 'recovery_required' : state.snapshot?.source ? null
    : run.phase || (run.stopping && !run.finished ? 'stopping' : null);
  state.attention = (!state.snapshot?.finished && state.hostFailure) || state.snapshot?.outcome?.reason || null;
  if (run.finished && !state.snapshot.finished) {
    // Keep the watcher until it reads the durable final result and coordination state.
    state.phase = run.final?.status || null;
  }
  if (state.runId !== previousRunId) remember($, state);
  invalidate($, state);
  if (!state.stream && state.uiReady) watch($, state);
}
function startNativeSync($, state) {
  if (!valid(state) || state.nativeTimer || state.snapshot?.finished) return;
  state.nativeTimer = $.clock.every(250, async () => {
    if (!valid(state) || state.nativeQuery) return;
    state.nativeQuery = true;
    try {
      const session = await $.session.id();
      if (!valid(state)) return;
      if (session !== state.session) { end($, state.session); return; }
      const run = observedRuns.get(state.session);
      if (!run || run === state.lastNative || !run.ready?.run_id
          || (!state.runId && run.ready.run_id === state.previousRunId)) return;
      state.lastNative = run;
      observe($, run);
    } catch {
      // A view with unconfirmed conversation ownership must not remain visible.
      if (valid(state)) end($, state.session);
    } finally { state.nativeQuery = false; }
  });
}
async function restore($, session, {show = false} = {}) {
  if (active && active.session !== session) {
    dispose(active); active = null; generation++;
    try { $.ui.invalidate('ui.render'); } catch {}
  }
  const stamp = generation;
  const run = await readRun($, session);
  if (generation !== stamp || await $.session.id() !== session) return null;
  if (!run) { dispose(active); active = null; generation++; return null; }
  let state = active;
  if (!state || state.session !== session || state.runId !== run.ready?.run_id) {
    state = replace(session, run);
    const saved = await $.store.get(STORE + session);
    if (!valid(state)) return null;
    if (saved?.version === 1 && saved.runId === state.runId) {
      state.hidden = Boolean(saved.hidden);
      if (saved.finalSnapshot && state.runId) {
        try { state.snapshot = validateSnapshot(saved.finalSnapshot, session, state.runId); } catch {}
      }
    }
  }
  state.nativeAgents = nativeAgents(run);
  if (!state.snapshot?.source) state.snapshot = initialSnapshot(run, session);
  startNativeSync($, state);
  if (show) await open($, state);
  else if (!state.hidden) ignore(open($, state));
  else state.uiReady = true;
  startPaneSync($, state);
  if (state.runId && state.uiReady) {
    if (state.snapshot?.finished) await readFinal($, state);
    else watch($, state);
  }
  return state;
}
async function page($, state, collection, offset = 0, itemId = null, fresh = false) {
  if (!valid(state) || !state.runId || !COLLECTIONS.has(collection)) return;
  const stamp = ++state.detailGeneration;
  const current = state.pages[collection] || state.snapshot?.[collection];
  const extra = ['--collection', collection];
  if (itemId != null) extra.push('--item-id', String(itemId));
  else {
    extra.push('--offset', String(Math.max(0, Number(offset) || 0)), '--limit', '8');
    if (!fresh && Number.isSafeInteger(current?.through_sequence)) extra.push('--through-sequence', String(current.through_sequence));
  }
  state.detailLoading = true; state.detailError = null; invalidate($, state);
  try {
    let value = await readOnce($, state, extra);
    const currentEnough = record => record.revision >= (state.snapshot?.revision || 0)
      && record.source.board_sequence >= (state.snapshot?.source?.board_sequence || 0);
    if (valid(state) && state.detailGeneration === stamp && !currentEnough(value)) value = await readOnce($, state, extra);
    if (!valid(state) || state.detailGeneration !== stamp) return;
    if (!currentEnough(value)) throw new Error('The board changed while this page was loading.');
    const records = value[collection];
    if (itemId != null) return {...records, view_revision: value.revision, view_board_sequence: value.source.board_sequence};
    const anchorChanged = current?.through_sequence !== records.through_sequence;
    const previous = anchorChanged ? (state.pagePrevious[collection] = {}) : (state.pagePrevious[collection] ||= {});
    if (!anchorChanged && current?.next_offset === records.offset && records.offset > (current.offset || 0)) {
      previous[records.offset] = current.offset || 0;
    }
    state.pages[collection] = {...records, previous_offset: previous[records.offset] ?? null,
      view_revision: value.revision, view_board_sequence: value.source.board_sequence};
    return state.pages[collection];
  } catch (error) { if (valid(state) && state.detailGeneration === stamp) state.detailError = 'This page is unavailable. Try again.'; }
  finally {
    if (valid(state) && state.detailGeneration === stamp) { state.detailLoading = false; invalidate($, state); }
  }
}
function navigateScroll($, state, to) {
  cancel(state.navigationTimer);
  try { $.ui.invalidate('ui.render'); } catch {}
  state.navigationTimer = $.clock.after(0, () => {
    if (!valid(state)) return;
    try { ignore($.ui.scroll({to, in: PANE, block: 'nearest'})); } catch {}
  });
}
async function refresh($, state) {
  const screen = state.screen;
  const collection = screen.collection || screen.kind;
  if (!COLLECTIONS.has(collection)) return;
  const offset = screen.id == null ? state.pages[collection]?.offset ?? screen.pageOffset ?? 0
    : screen.pageOffset ?? 0;
  const records = await page($, state, collection, offset, screen.id ?? null, true);
  if (!valid(state) || state.screen !== screen || !records || screen.id == null) return;
  const item = records.items.find(item => String(item.id) === String(screen.id));
  if (!item) {
    state.detailError = 'This item is no longer available. The previous details remain below.';
    invalidate($, state);
    return;
  }
  screen.item = item;
  screen.view_revision = records.view_revision;
  screen.view_board_sequence = records.view_board_sequence;
  invalidate($, state);
}
function actions($, state) {
  const navigate = (screen, returnKey) => {
    state.history.push({...state.screen, returnKey}); state.screen = screen;
    state.detailError = null; navigateScroll($, state, 'start');
  };
  return {
    details: () => { if (valid(state)) navigate({kind: 'details'}, 'board-details'); },
    select: (kind, id) => {
      if (!valid(state)) return;
      const collection = kind === 'task' ? 'tasks' : kind === 'contribution' || kind === 'shared' ? 'shared' : kind;
      const group = (state.screen.kind === collection ? state.pages[collection] : null) || state.snapshot?.[collection];
      const item = group?.items.find(value => String(value.id) === String(id));
      const key = id == null ? (state.screen.kind === 'details' ? 'detail-' : 'view-all-') + collection
        : (collection === 'tasks' ? 'task-' : 'shared-') + id;
      navigate({kind, id, item, collection, pageOffset: group?.offset || 0,
        view_revision: group?.view_revision ?? state.snapshot?.revision,
        view_board_sequence: group?.view_board_sequence ?? state.snapshot?.source?.board_sequence}, key);
      if (id == null && COLLECTIONS.has(collection)) ignore(page($, state, collection, 0, null, true));
      else if (id != null && !item && COLLECTIONS.has(collection)) ignore(refresh($, state));
    },
    back: () => {
      if (!valid(state)) return;
      state.detailGeneration++; state.detailLoading = false;
      state.screen = state.history.pop() || {kind: 'overview'};
      navigateScroll($, state, state.screen.returnKey ? {key: state.screen.returnKey} : 'start');
    },
    page: (collection, offset) => { ignore(page($, state, collection, offset)); },
    refresh: () => { if (valid(state)) ignore(refresh($, state)); },
    retryFinish: () => {
      if (!valid(state) || !state.canRetryFinish || state.retryingFinish || !canRetry(state.session)) return;
      state.retryingFinish = true; invalidate($, state);
      ignore((async () => {
        try {
          if (await $.session.id() !== state.session || !valid(state)) return;
          await retryFinishing(state.session);
          if (valid(state)) {
            const run = await readRun($, state.session);
            if (run && valid(state)) observe($, run);
          }
        } catch (error) {
          if (valid(state)) state.attention = cleanText(error?.message || error, 600);
        } finally {
          if (valid(state)) { state.retryingFinish = false; invalidate($, state); }
        }
      })());
    },
    hide: () => {
      if (!valid(state)) return;
      state.hidden = true; state.paneShown = false; remember($, state);
      ignore($.ui.close({id: PANE})); restart($, state); invalidate($, state);
    },
    show: () => { if (valid(state)) { ignore(open($, state)); state.retries = 0; restart($, state); } },
  };
}
function draw($, e, next, compact) {
  const state = active;
  if (!state || !valid(state)) return next(e);
  const viewed = e.props?.view?.agentId;
  if (viewed && !state.nativeAgents.some(agent => agent.id === viewed)) return next(e);
  const native = observedRuns.get(state.session);
  const screen = state.screen;
  const pageSource = state.pages[screen.kind] || (screen.item ? screen : null);
  const rendering = {...state, selectedAgentId: viewed,
    detailStale: pageSource && (pageSource.view_revision < state.snapshot?.revision
      || pageSource.view_board_sequence < state.snapshot?.source?.board_sequence),
    detailRevision: pageSource?.view_revision,
    nativeAgents: native ? nativeAgents(native) : state.nativeAgents};
  try {
    const components = $.ui.resolve(e);
    const result = compact ? renderCompact(components, e, rendering, actions($, state)) : renderBoard(components, e, rendering, actions($, state));
    return result ?? next(e);
  } catch {
    // Fall back using only native primitives; no observer or lifecycle action runs here.
    try { const {Text} = $.ui.resolve(e); return Text({children: 'DeLM board unavailable. Use /delm-status.'}); }
    catch { return next(e); }
  }
}
function begin($, session, previousRunId = null) {
    const state = replace(session, null);
    state.previousRunId = previousRunId;
    ignore(open($, state));
    startNativeSync($, state);
}
function failure($, session, error) {
    if (active?.session === session) {
      active.phase = 'error'; active.attention = cleanText(error?.message || error, 600);
      cancel(active.nativeTimer); active.nativeTimer = null;
      cancel(active.paneTimer); active.paneTimer = null;
      invalidate($, active);
    }
}
async function status($, session) {
    const state = await restore($, session, {show: true});
    if (!state) return {text: 'No DeLM run in this conversation.'};
    if (state.uiReady && state.paneShown) return {};
    return {text: boardSummary(state)};
}
function end($, session) {
    if (active?.session !== session) return;
    dispose(active); active = null; generation++;
    try { ignore($.ui.close({id: PANE})); } catch {}
    try { $.ui.invalidate('ui.render'); } catch {}
}
async function clearForeignView($) {
  const state = active, stamp = generation;
  try {
    const session = await $.session.id();
    if (generation !== stamp) return null;
    if (state && valid(state) && state.session !== session) end($, state.session);
    return session;
  } catch {
    if (state && valid(state)) end($, state.session);
    return null;
  }
}
export function registerBoard(on, {lookup, retryFinish} = {}) {
  dispose(active); active = null; generation++;
  observedRuns.clear(); retryFinishActions.clear(); lookupOverride = lookup; retryFinishAction = retryFinish;
  on('ui.render', {component: 'Pane', requestId: PANE}, ($, e, next) => draw($, e, next, false));
  on('ui.render', {component: 'AbovePrompt'}, ($, e, next) => draw($, e, next, true));
  on('ui.close', {id: PANE}, async ($, e, next) => {
    const state = active;
    const result = await next(e);
    if (state && valid(state) && !result?.deny) {
      state.paneShown = false;
      if (e.origin?.kind === 'person') { state.hidden = true; remember($, state); restart($, state); }
      invalidate($, state);
    }
    return result;
  });
  on('command.*', async ($, e, next) => {
    const session = await clearForeignView($);
    if (!session || !next.is('command.run', e) || e.command !== 'delm:run') return next(e);
    try {
      const current = await readRun($, session);
      if (e.args?.trim() && (!current || current.finished)) begin($, session, current?.ready?.run_id);
    } catch {}
    const result = await next(e);
    try {
      if (result.exitCode) failure($, session, result.text);
      else { const run = await readRun($, session); if (run) observe($, run); }
    } catch {}
    return result;
  });
  on('command.run', {command: 'delm-status'}, async ($) => {
    try { return await status($, await $.session.id()); }
    catch { return {text: 'DeLM could not read this board. Your run controls remain available.'}; }
  });
  on('session.*', async ($, e, next) => {
    const session = await clearForeignView($);
    if (next.is('session.start', e)) {
      const result = await next(e);
      try {
        const current = await $.session.id();
        void restore($, current).catch(() => {});
      } catch {}
      return result;
    }
    if (next.is('session.end', e)) {
      try { end($, e.sessionId || session); } catch {}
    }
    return next(e);
  });
  return {begin, observe, failure, restore, status, end};
}
