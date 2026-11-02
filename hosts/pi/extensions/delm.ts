// DeLM for Pi: the parent session's extension. It starts the shared runtime,
// forks the conversation into two private Pi workers, relays their native
// events to the controller, applies the controller's actions, forwards the
// user's updates, and reports the delivered result.
import type {ExtensionAPI, ExtensionContext} from '@earendil-works/pi-coding-agent';
import {SessionManager} from '@earendil-works/pi-coding-agent';
import {appendFileSync, mkdtempSync, readFileSync, realpathSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {randomUUID} from 'node:crypto';
import {execFile} from 'node:child_process';
import {PiProcess, type RpcEvent} from '../lib/rpc.ts';
import {TicketServer, bridgeRoot, type TicketRequest, type ReleaseRequest} from '../lib/bridge.ts';
import {request, startBridge, type Action, type Bridge} from '../lib/control.ts';
import {commandOutcome, duplicateTask, duplicateTaskReason, explainModelError, hostNotes, launchPrompt, turnReason, updateText} from '../lib/protocol.ts';
import {BoardObserver, fetchCollection, renderBoard, renderCollection, type View} from '../lib/board.ts';
import {existsSync, readdirSync} from 'node:fs';
import {homedir} from 'node:os';

type Worker = {
  slot: number;
  agentId: string;
  process: PiProcess;
  /** The run this worker belongs to; late events after that run ended are ignored. */
  run: Run;
  /** Events are handled one at a time per worker, in arrival order. */
  chain: Promise<void>;
  turn: string | null;
  deliveredRevision: number;
  status: 'running' | 'completed' | 'failed' | 'killed';
  stopPromise: Promise<unknown> | null;
  lastError: string | null;
};

type Run = {
  sessionId: string;
  log: (line: string) => void;
  modelErrorReported: boolean;
  observer: BoardObserver | null;
  view: View | null;
  boardHidden: boolean;
  task: string;
  bridge: Bridge;
  revision: number;
  workers: Worker[];
  tickets: TicketServer[];
  /** Peer 2's launch waits for the first shared task; an update releases it early. */
  launchGate: {release: () => Promise<void>} | null;
  bridgeExited: boolean;
  finalization: {generation: number; revision: number; intent: string} | null;
  settling: Promise<void> | null;
  finished: boolean;
  final: Action | null;
  /** Ids of actions already applied; the runtime sends each result's actions twice (reply and stdout). */
  applied: Set<string>;
  /** Set until both peers are bound and peer 1 is prompted; updates wait for it. */
  launching: boolean;
  /** Set while an update is being registered; a step sent meanwhile would cite a stale revision. */
  updating: boolean;
  failure: string | null;
  stopRequested: boolean;
  sessionDir: string;
};

const PACKAGE_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function runtimeExecutable(): string {
  return process.env.DELM_PI_RUNTIME || join(PACKAGE_ROOT, 'bin', 'delm');
}

function workerPolicy(): string {
  const policy = readFileSync(join(PACKAGE_ROOT, 'worker.md'), 'utf8');
  return policy.trimEnd() + '\n\n' + hostNotes() + '\n';
}

// Pi invokes this factory for every runtime it creates (startup, /new, /resume,
// /reload) and once per installed copy (the package plus a `-e` path). The most
// recent invocation owns the session: earlier copies in the same process hand
// their commands to it and ignore events, so nothing runs twice.
const ACTIVE = Symbol.for('delm.pi.parent.active');
type CommandHandler = (args: string | undefined, ctx: ExtensionContext) => Promise<void>;
type Ownership = {token: object; commands: Map<string, CommandHandler>};

export default function (pi: ExtensionAPI) {
  if (process.env.DELM_PI_WORKER) return;   // A worker never starts a nested run.
  const shared = globalThis as unknown as Record<symbol, Ownership | undefined>;
  const ownership: Ownership = {token: {}, commands: new Map()};
  shared[ACTIVE] = ownership;
  const active = () => shared[ACTIVE]?.token === ownership.token;
  const command = (name: string, description: string, handler: CommandHandler) => {
    ownership.commands.set(name, handler);
    pi.registerCommand(name, {description, handler: (args, ctx) => (shared[ACTIVE]?.commands.get(name) || handler)(args, ctx)});
  };

  let run: Run | null = null;
  let lastFinished: Run | null = null;
  let ctxRef: ExtensionContext | null = null;

  const notify = (message: string, level: 'info' | 'warning' | 'error' = 'info') => {
    try { ctxRef?.ui.notify(message, level); } catch { /* No UI in print mode. */ }
  };
  const status = (text: string | undefined) => {
    try { ctxRef?.ui.setStatus('delm', text); } catch { /* No UI in print mode. */ }
  };

  async function control(op: string, fields: Record<string, unknown> = {}, timeoutMs?: number) {
    if (!run) throw new Error('No DeLM run is active.');
    return request(run.bridge.ready, op, fields, timeoutMs);
  }

  /** The same request bound to one run, for callbacks that may outlive it. */
  function controlOn(current: Run, op: string, fields: Record<string, unknown> = {}, timeoutMs?: number) {
    if (current.finished || run !== current) return Promise.reject(new Error('This DeLM run has ended.'));
    return request(current.bridge.ready, op, fields, timeoutMs);
  }

  // ---- actions from the controller ------------------------------------------

  async function applyActions(list: Action[], current: Run | null = run) {
    if (!current || run !== current) return;
    for (const action of list) {
      if (current.finished && action.type !== 'final') return;
      if (action.id) {
        if (current.applied.has(action.id)) continue;
        current.applied.add(action.id);
      }
      try {
        await applyAction(current, action);
      } catch (error) {
        current.failure = String((error as Error).message || error);
        notify('DeLM: ' + current.failure, 'error');
      }
      if (current.finished) return;
    }
  }

  async function applyAction(current: Run, action: Action) {
    if (run !== current) return;
    switch (action.type) {
      case 'candidate':
      case 'stop': {
        const finalization = action.finalization as Run['finalization'];
        if (!finalization) throw new Error('DeLM received an incompatible finalization contract.');
        if (finalization.revision < current.revision) return;
        current.finalization = finalization;
        status(finalization.intent === 'deliver' ? 'DeLM: delivering' : 'DeLM: stopping');
        void settle();
        return;
      }
      case 'resume':
      case 'context': {
        const target = current.workers.find(w => w.agentId === action.agent_id);
        if (!target || current.finalization) return;
        if (Number.isSafeInteger(action.revision)) current.revision = Math.max(current.revision, action.revision as number);
        await deliver(target, String(action.message), action.revision as number, action.type === 'resume');
        return;
      }
      case 'completion_intent':
        return;
      case 'final': {
        current.finished = true;
        current.final = action;
        status(undefined);
        const outcome = String(action.status);
        const delivered = outcome === 'complete' || outcome === 'delivered';
        if (delivered) notify('DeLM delivered the result into your project.');
        else if (current.modelErrorReported) notify('DeLM stopped without changing your project. Fix the model access above, then run /delm:run again.', 'warning');
        else {
          const reason = String(action.reason || current.failure || '').trim();
          notify('DeLM ' + outcome.replaceAll('_', ' ') + (reason ? ': ' + reason : '') + '. Worker log: ' + join(current.sessionDir, 'delm-pi.log'), 'warning');
        }
        const verification = (action.delivery as {verification_required?: boolean} | undefined)?.verification_required
          ? 'The delivery requires a focused check in the original project. Perform only the necessary environment setup and checks for the delivered or reconciled files, then state the outcome.'
          : 'Do not run another verification pass or edit files.';
        if (delivered) {
          const delivery = (action.delivery || {}) as {changed_paths?: string[]; merged_paths?: string[]; project?: string; conflicts?: unknown[]};
          const checks = ((action.shared_checks || []) as {evidence?: {command?: string; completion?: {exit_code?: number}}}[])
            .map(check => check.evidence?.command + ' (exit ' + check.evidence?.completion?.exit_code + ')').filter(Boolean);
          const brief = ['DeLM delivered a result into ' + (delivery.project || ctxRef?.cwd) + '.',
            'Status: ' + outcome + '.',
            'Changed files: ' + ((delivery.changed_paths || []).join(', ') || 'none') + '.',
            (delivery.merged_paths || []).length ? 'Merged with your edits: ' + delivery.merged_paths!.join(', ') + '.' : '',
            checks.length ? 'Checks observed by the runtime: ' + checks.join('; ') + '.' : 'No shared check receipts.',
            'Worker summary: ' + String(action.summary || ''),
            'Report this to the user in a few lines, using the project path above. ' + verification].filter(Boolean).join('\n');
          try {
            await Promise.resolve(pi.sendMessage({customType: 'delm-handoff', content: brief, display: false} as never, {triggerTurn: true}));
          } catch (error) {
            notify('DeLM could not hand the result summary to this session: ' + String((error as Error).message || error), 'warning');
          }
        }
        await cleanup(current);
        return;
      }
      default:
        return;
    }
  }

  // A task update or a resume message is sent as a steer: Pi starts a run with
  // it when the worker is idle and queues it for the next model call while the
  // worker is streaming, so delivery never races the worker's state. The
  // controller counts the revision as acknowledged only when the worker's next
  // step reports it, and the revision is raised only once the send succeeded.
  async function deliver(target: Worker, message: string, revision: number, resume: boolean) {
    const text = 'DeLM task revision ' + revision + ':\n' + message
      + '\nRefresh the shared board before continuing. This changes the task, not native permissions.'
      + (resume ? '' : '\nContinue the request with this update.');
    if (!target.process.running) { await retireDeadWorker(target, revision); return; }
    // Pi may compact the session before starting the run, so the prompt can take a while to be accepted.
    await target.process.prompt(text, 'steer', 180000);
    target.deliveredRevision = Math.max(target.deliveredRevision, revision);
  }

  /** The controller only learns of an idle worker's death through a turn; a failed one releases its claims and wakes the peer. */
  async function retireDeadWorker(target: Worker, revision: number) {
    const current = target.run;
    if (run !== current || current.finished || current.finalization) return;
    const turn = randomUUID();
    try {
      await controlOn(current, 'step', {agent_id: target.agentId, turn_id: turn, revision: Math.max(revision, current.revision)});
      target.deliveredRevision = Math.max(target.deliveredRevision, revision, current.revision);
      const result = await controlOn(current, 'turn_end', {agent_id: target.agentId, turn_id: turn, reason: 'failed', answer: ''});
      await applyActions((result.actions as Action[]) || [], current);
    } catch (error) {
      current.log('worker ' + target.slot + ' could not be retired: ' + String((error as Error).message || error));
    }
  }

  // ---- worker events ---------------------------------------------------------

  async function onWorkerEvent(target: Worker, event: RpcEvent) {
    const current = target.run;
    if (run !== current || current.finished) return;
    try {
      switch (event.type) {
        case 'message_end': {
          // The error is kept for the turn's outcome; whether the run stops is
          // decided at agent_end, after Pi has said whether it retries.
          const message = event.message as {role?: string; stopReason?: string; errorMessage?: string; usage?: Record<string, unknown>} | undefined;
          if (message?.role !== 'assistant') return;
          if (message.usage) current.log('worker ' + target.slot + ' usage ' + JSON.stringify(message.usage));
          if (message.stopReason !== 'error') { target.lastError = null; return; }   // a retried call succeeded
          target.lastError = String(message.errorMessage || 'the model returned an error');
          if (accountError(target.lastError)) await stopForModelError(current, target, target.lastError);
          return;
        }
        case 'agent_end': {
          if (!target.lastError) return;
          if (event.willRetry === true) { current.log('worker ' + target.slot + ' model error, Pi retries: ' + target.lastError.slice(0, 200)); return; }
          if (recoverableError(target.lastError)) { current.log('worker ' + target.slot + ' recoverable model error: ' + target.lastError.slice(0, 200)); return; }
          await stopForModelError(current, target, target.lastError);
          return;
        }
        case 'extension_ui_request': {
          // A worker's own extensions may open a dialog; nobody can answer it there.
          target.process.cancelUiRequest(event.id);
          current.log('worker ' + target.slot + ' cancelled a UI request from its extensions');
          return;
        }
        case 'agent_start': {
          // Pi starts a new model loop within one run when it drains a steer or
          // recovers from context overflow; the native turn spans the whole run,
          // from the first agent_start to agent_settled.
          if (target.turn) return;
          target.turn = randomUUID();
          // A worker still behind the request revision registers its turn on the first step after the update lands.
          if (current.updating || target.deliveredRevision !== current.revision) return;
          await controlOn(current, 'step', {agent_id: target.agentId, turn_id: target.turn, revision: target.deliveredRevision});
          return;
        }
        case 'turn_start': {
          // Each model call within the run is a native step. After an update
          // was steered into the run, the next call has it in context, so this
          // is where the worker acknowledges the new revision (as Claude does
          // on turn.step). A stale worker is skipped until its update lands.
          if (!target.turn || current.updating || target.deliveredRevision !== current.revision) return;
          await controlOn(current, 'step', {agent_id: target.agentId, turn_id: target.turn, revision: target.deliveredRevision});
          return;
        }
        case 'tool_execution_start': {
          if (event.toolName !== 'bash' || !target.turn) return;
          const args = (event.args || {}) as {command?: string};
          try {
            await controlOn(current, 'command_start', {
              agent_id: target.agentId, turn_id: target.turn, call_id: String(event.toolCallId),
              command: String(args.command ?? ''), cwd: current.bridge.ready.workers[target.slot - 1].cwd,
            });
          } catch (error) {
            // Unrecorded evidence only means this command cannot back a check receipt.
            current.log('worker ' + target.slot + ' command_start refused: ' + String((error as Error).message || error));
          }
          return;
        }
        case 'tool_execution_end': {
          if (event.toolName !== 'bash' || !target.turn) return;
          const outcome = commandOutcome(event as {result?: unknown; isError?: boolean});
          try {
            await controlOn(current, 'command_end', {
              agent_id: target.agentId, call_id: String(event.toolCallId), result_ref: String(event.toolCallId),
              exit_code: outcome.exit_code, is_error: outcome.is_error, interrupted: outcome.interrupted,
              timed_out: outcome.timed_out, background_task_id: null,
            });
          } catch (error) {
            current.log('worker ' + target.slot + ' command_end refused: ' + String((error as Error).message || error));
          }
          return;
        }
        case 'agent_settled': {
          if (!target.turn) return;
          const turn = target.turn;
          target.turn = null;
          const answer = (await target.process.lastAssistantText().catch(() => null)) || '';
          const reason = target.lastError && !answer ? 'failed' : turnReason(event as {aborted?: boolean}, !target.process.running);
          target.lastError = null;
          const result = await controlOn(current, 'turn_end', {agent_id: target.agentId, turn_id: turn, reason, answer});
          await applyActions((result.actions as Action[]) || [], current);
          return;
        }
        default:
          return;
      }
    } catch (error) {
      const message = String((error as Error).message || error);
      // A step that cites a revision the controller just moved past is retried on the worker's next model call.
      if (/acknowledge the current task revision/i.test(message)) { current.log('worker ' + target.slot + ' step deferred: ' + message); return; }
      if (run === current && !current.finalization && !current.finished) {
        current.failure = message;
        notify('DeLM: ' + current.failure, 'error');
      }
    }
  }

  /** Login and quota refusals are about the account; stop at once, before Pi retries them. */
  function accountError(raw: string): boolean {
    return /out of extra usage|specified API usage limits?|spend(ing)? limit|regain access on|credit balance|insufficient_quota|\b401\b|authentication|invalid[ -_]?(x-)?api[ -_]?key|unauthorized|token expired|not logged in|model not found|does not exist|unsupported model|not_found_error|^\s*404\b/i.test(raw);
  }

  /** Context overflow is recovered by Pi's own compaction; the run goes on. */
  function recoverableError(raw: string): boolean {
    return /context|too long|too many tokens|token limit|prompt is too large|maximum.*length/i.test(raw);
  }

  // The workers use the same provider login as this session, so a refusal is
  // almost always about the account, not the task. Stop before the controller
  // spends resumes on empty turns, and tell the user what to change.
  async function stopForModelError(current: Run, target: Worker, error: string) {
    if (run !== current || current.finished || current.finalization || current.modelErrorReported) return;
    current.modelErrorReported = true;
    current.failure = explainModelError(error);
    notify('DeLM stopped: peer ' + target.slot + '\'s model request was rejected. ' + current.failure
      + ' Nothing in your project was changed.', 'error');
    try {
      const cancelled = await controlOn(current, 'cancel', {reason: 'Peer model error'});
      await applyActions((cancelled.actions as Action[]) || [], current);
    } catch (cancelError) {
      if (current.bridgeExited) await abandon(current, 'the DeLM runtime exited');
      else notify('DeLM could not stop cleanly: ' + String((cancelError as Error).message || cancelError), 'error');
    }
  }

  async function onWorkerExit(target: Worker) {
    const current = target.run;
    if (run !== current || current.finished) return;
    if (target.status === 'running') target.status = current.finalization ? 'killed' : 'failed';
    if (target.turn) {
      const turn = target.turn;
      target.turn = null;
      try {
        const result = await controlOn(current, 'turn_end', {agent_id: target.agentId, turn_id: turn, reason: 'failed', answer: ''});
        await applyActions((result.actions as Action[]) || [], current);
      } catch { /* The controller retires the turn at settlement. */ }
    } else if (!current.finalization && !current.stopRequested) {
      // An idle worker that disappears is not seen by the controller until it is resumed.
      current.log('worker ' + target.slot + ' exited while idle: ' + (target.process.stderr.trim().slice(-300) || 'no stderr'));
      notify('DeLM: peer ' + target.slot + ' exited unexpectedly. See the worker log: ' + join(current.sessionDir, 'delm-pi.log'), 'warning');
      await retireDeadWorker(target, current.revision);
    }
    if (run === current && !current.finished && !current.finalization && current.workers.every(w => !w.process.running)) {
      current.failure = 'Both peers exited';
      try {
        const cancelled = await controlOn(current, 'cancel', {reason: 'Both native peers exited'});
        await applyActions((cancelled.actions as Action[]) || [], current);
      } catch { if (current.bridgeExited) await abandon(current, 'both peers and the DeLM runtime exited'); }
    }
  }

  /**
   * The runtime is gone or unreachable, so no delivery or cancel can reach it.
   * The session still holds the run's capability token, so it saves the
   * unfinished changes itself through the tokened recovery path and releases
   * the project; only if that fails is the user pointed at /delm:recover.
   */
  async function abandon(current: Run, reason: string) {
    if (run !== current || current.finished) return;
    current.finished = true;
    current.failure = current.failure || reason;
    const agents = await stopWorkers(current);
    if (!current.bridgeExited) {
      // Recovery needs the bridge process gone; an unreachable one is ended here.
      try { current.bridge.process.kill('SIGTERM'); } catch { /* already gone */ }
      await Promise.race([current.bridge.exited, new Promise(resolve => setTimeout(resolve, 5000))]);
      if (!current.bridgeExited) { try { current.bridge.process.kill('SIGKILL'); } catch { /* gone */ } await Promise.race([current.bridge.exited, new Promise(resolve => setTimeout(resolve, 2000))]); }
    }
    const saved = await recoverRun(current.bridge.ready.run_id, {
      token: current.bridge.ready.token, session_id: current.sessionId, background_tasks_stopped: true, agents,
    });
    const log = ' Worker log: ' + join(current.sessionDir, 'delm-pi.log');
    if (saved.ok) notify('DeLM run interrupted: ' + reason + '. ' + saved.message + ' Your project is unchanged.' + log, 'error');
    else notify('DeLM run interrupted: ' + reason + '. Its private workspaces are kept but could not be saved from this session (' + saved.message
      + '). Start a new Pi session in this project and run /delm:recover.' + log, 'error');
    await cleanup(current);
    // The record may stay unfinished when saving failed; the observer must not poll it forever.
    setTimeout(() => { if (lastFinished === current || run !== current) { paintBoard(current); stopBoard(current); } }, 1500);
  }

  /** Runs `delm pi recover` for one run; `body` is `{}` for the tokenless path or the session's capability for the tokened one. */
  function recoverRun(id: string, body: Record<string, unknown>): Promise<{ok: boolean; message: string}> {
    return new Promise(resolve => {
      const child = execFile(runtimeExecutable(), ['pi', 'recover', '--run-id', id], {timeout: 120000}, (error, stdout, stderr) => {
        if (error) return resolve({ok: false, message: String(stderr || error.message).trim().slice(0, 300)});
        let result: {status?: string; recovery?: {recovery?: string}} = {};
        try { result = JSON.parse(String(stdout).trim().split('\n').at(-1) || '{}'); } catch { /* raw */ }
        resolve({ok: true, message: 'Run ' + id.slice(0, 8) + ' ' + String(result.status || 'recovered') + '.'
          + (result.recovery?.recovery ? ' Unfinished changes were saved to ' + result.recovery.recovery + '.' : ' Nothing needed saving.')});
      });
      child.stdin?.on('error', () => { /* The exit callback reports the outcome. */ });
      child.stdin?.end(JSON.stringify(body) + '\n');
    });
  }

  // ---- launch ----------------------------------------------------------------

  // `pi auth check` refreshes and validates the provider login the workers will
  // inherit. It cannot see usage quotas; those surface on the first request.
  function checkLogin(model: {provider: string; id: string} | null): Promise<void> {
    if (!model) return Promise.reject(new Error('No model is selected in this Pi session. Choose one with /model, then run /delm:run again.'));
    if (process.env.DELM_PI_COMMAND) return Promise.resolve();   // a scripted host has no login
    return new Promise((resolve, reject) => {
      execFile(process.env.DELM_PI_COMMAND || 'pi', ['auth', 'check', '--provider', model.provider, '--model', model.id, '--json'],
        {timeout: 30000, env: process.env}, (error, stdout, stderr) => {
          if (!error) return resolve();
          const detail = String(stdout || stderr || error.message).trim().replace(/\s+/g, ' ').slice(0, 300);
          reject(new Error('Pi has no working login for ' + model.provider + '/' + model.id + ' (' + detail + '). Run /login in Pi, then /delm:run again.'));
        });
    });
  }

  let starting = false;

  async function launch(ctx: ExtensionContext, task: string) {
    if (starting) throw new Error('DeLM is already starting a run; wait for it to report.');
    starting = true;
    try { await launchInner(ctx, task); } finally { starting = false; }
  }

  async function launchInner(ctx: ExtensionContext, task: string) {
    if (lastFinished) { stopBoard(lastFinished); lastFinished = null; try { ctx.ui.setWidget('delm-board', undefined); } catch { /* no UI */ } }
    const executable = runtimeExecutable();
    const selected = ctx.model ? {provider: (ctx.model as {provider: string}).provider, id: (ctx.model as {id: string}).id} : null;
    await checkLogin(selected);
    const sessionId = ctx.sessionManager.getSessionId();
    const parentFile = ctx.sessionManager.getSessionFile();
    const bridge = await startBridge(executable, {
      project: ctx.cwd, session_id: sessionId, task, host_version: null, package_root: PACKAGE_ROOT,
    }, actions => { void applyActions(actions); });
    if (bridge.ready.session_id !== sessionId) {
      try { await request(bridge.ready, 'cancel', {reason: 'Session mismatch at launch'}, 5000); } catch { bridge.process.kill('SIGTERM'); }
      throw new Error('DeLM returned workspaces for another conversation.');
    }
    const sessionDir = mkdtempSync(join(tmpdir(), 'delm-pi-sessions-'));
    const logFile = join(sessionDir, 'delm-pi.log');
    const log = (line: string) => { try { appendFileSync(logFile, new Date().toISOString() + ' ' + line + '\n'); } catch { /* Diagnostics only. */ } };
    run = {
      sessionId, task, bridge, revision: bridge.ready.revision, workers: [], tickets: [], finalization: null,
      settling: null, finished: false, final: null, applied: new Set(), launching: true, updating: false, failure: null, stopRequested: false, sessionDir, log,
      modelErrorReported: false, observer: null, view: null, boardHidden: false, launchGate: null, bridgeExited: false,
    };
    const current = run;
    startBoard(ctx);
    log('run ' + bridge.ready.run_id + ' project ' + ctx.cwd + ' sessions ' + sessionDir + ' runtime pid ' + bridge.process.pid);
    void bridge.exited.then(async code => {
      current.bridgeExited = true;
      if (run !== current || current.finished) return;
      // The runtime announces a normal end with a final action before exiting; give that a moment to arrive.
      await new Promise(resolve => setTimeout(resolve, 1500));
      if (run !== current || current.finished) return;
      current.log('runtime exited with code ' + code + ' before the run ended: ' + current.bridge.stderr().trim().slice(-500));
      await abandon(current, 'the DeLM runtime exited (code ' + code + ')');
    });
    const policyFile = join(sessionDir, 'worker-policy.md');
    writeFileSync(policyFile, workerPolicy(), {mode: 0o600});
    const model = selected ? `${selected.provider}/${selected.id}` : null;
    const thinking = pi.getThinkingLevel();
    const root = bridgeRoot();
    const pendingActions: Action[] = [];
    try {
      for (const slot of [1, 2]) {
        const cwd = bridge.ready.workers[slot - 1].cwd;
        const bridgePath = join(root, bridge.ready.run_id + '-' + slot + '.sock');
        const tickets = new TicketServer(bridgePath, message => ticketHandler(slot, message),
          ticket => { void controlOn(current, 'revoke', {ticket}).catch(() => undefined); });
        await tickets.listen();
        run.tickets.push(tickets);
        let sessionFile: string | null = null;
        if (parentFile) {
          try {
            const forked = SessionManager.forkFrom(parentFile, cwd, sessionDir);
            sessionFile = forked.getSessionFile() ?? null;
          } catch { sessionFile = null; }
        }
        const args = ['--mode', 'rpc', '--session-dir', sessionDir,
          ...(sessionFile ? ['--session', sessionFile] : []),
          '--append-system-prompt', policyFile,
          '--extension', join(PACKAGE_ROOT, 'extensions', 'delm-worker.ts'),
          ...(model ? ['--model', model] : []), ...(thinking ? ['--thinking', String(thinking)] : [])];
        const child = new PiProcess(process.env.DELM_PI_COMMAND || 'pi', args, {
          cwd, env: {...process.env, DELM_PI_WORKER: String(slot), DELM_PI_BRIDGE: bridgePath, DELM_PI_RUNTIME: executable,
            DELM_PI_RUN: bridge.ready.run_id},
        });
        const target: Worker = {
          slot, agentId: 'pi-' + slot + '-' + (child.pid || randomUUID().slice(0, 8)), process: child, run: current, chain: Promise.resolve(),
          turn: null, deliveredRevision: bridge.ready.revision, status: 'running', stopPromise: null, lastError: null,
        };
        current.workers.push(target);
        let deltas = 0;
        child.onEvent(event => {
          if (event.type === 'message_update') {
            // A heartbeat every 200 stream deltas shows a long response is still flowing.
            if (++deltas % 200 === 0) current.log('worker ' + slot + ' streaming: ' + deltas + ' deltas');
          } else if (event.type !== 'tool_execution_update') {
            current.log('worker ' + slot + ' ' + JSON.stringify(event).slice(0, 2000));
          }
          // One event at a time per worker keeps command_start before command_end and both before turn_end.
          target.chain = target.chain.then(() => onWorkerEvent(target, event)).catch(() => undefined);
        });
        current.log('worker ' + slot + ' pid ' + child.pid + ' args ' + JSON.stringify(args));
        void child.exited.then(() => { target.chain = target.chain.then(() => onWorkerExit(target)).catch(() => undefined); });
        const bound = await control('bind', {slot, agent_id: target.agentId});
        await control('configure_scopes', {agent_id: target.agentId, scopes: [{path: cwd, access: 'write'}]});
        pendingActions.push(...((bound.actions as Action[]) || []));
      }
      const [first, second] = current.workers;
      await first.process.prompt(launchPrompt(task, 1));
      current.launching = false;
      gateSecondPeer(current, second, task);
      if (pendingActions.length) await applyActions(pendingActions, current);
    } catch (error) {
      try { await controlOn(current, 'cancel', {reason: 'Native peer launch failed'}); } catch { /* Recovery retains the workspace. */ }
      // Whatever the cancel achieved, this session must not stay attached to a run that never started.
      current.finished = true;
      await stopWorkers(current);
      await cleanup(current);
      throw error;
    }
    status('DeLM: 2 peers working');
  }

  async function ticketHandler(slot: number, message: TicketRequest | ReleaseRequest) {
    if (!run || run.finished) return {error: 'DeLM run is not active.'};
    if ('release' in message) {
      try { await control('revoke', {ticket: message.release}); } catch { /* Already consumed. */ }
      return {released: true};
    }
    const target = run.workers[slot - 1];
    if (!target?.turn) return {error: 'The worker has no active native turn.'};
    if (message.tool === 'delm_task_create') {
      const duplicate = await duplicateOpenTask(run, message.arguments.title);
      if (duplicate) return {error: duplicate};
    }
    // The worker waits 45 s for a ticket; the duplicate check (8 s) and the reservation (25 s) stay inside that.
    const reservation = await control('reserve', {
      agent_id: target.agentId, turn_id: target.turn, call_id: message.call_id, tool: message.tool, arguments: message.arguments,
    }, 25000);
    if (typeof reservation.ticket !== 'string') return {error: 'Missing native invocation ticket.'};
    return {socket: run.bridge.ready.socket, ticket: reservation.ticket};
  }

  const LAUNCH_GATE_MS = 40000;

  /** Peer 2 is prompted once peer 1 has shared a task, when an update arrives, or after the bounded wait. */
  function gateSecondPeer(current: Run, second: Worker, task: string) {
    let release: () => void = () => {};
    const released = new Promise<void>(resolve => { release = resolve; });
    const launched = (async () => {
      const planned = await firstTaskShared(current, released);
      current.launchGate = null;
      if (run !== current || current.finished) return;
      current.log('worker 2 prompted ' + (planned ? 'after the first shared task' : 'without a shared task'));
      try { await second.process.prompt(launchPrompt(task, 2, planned)); }
      catch (error) { current.log('worker 2 launch prompt failed: ' + String((error as Error).message || error)); }
    })();
    current.launchGate = {release: () => { release(); return launched; }};
  }

  async function firstTaskShared(current: Run, released: Promise<void>): Promise<boolean> {
    const deadline = Date.now() + LAUNCH_GATE_MS;
    let open = true;
    void released.then(() => { open = false; });
    const shared = async () => {
      if ((current.view?.tasks.total ?? 0) > 0) return true;
      try {
        const page = await fetchCollection(runtimeExecutable(), current.bridge.ready.run_id, current.sessionId, 'tasks', 1);
        return page.tasks.total > 0;
      } catch (error) { current.log('launch gate check skipped: ' + String((error as Error).message || error)); return false; }
    };
    while (open && Date.now() < deadline && run === current && !current.finished) {
      if (await shared()) return true;
      await Promise.race([released, new Promise(resolve => setTimeout(resolve, 1000))]);
    }
    // Released early or timed out: the board may have filled meanwhile.
    return run === current && !current.finished ? shared() : false;
  }

  /** Peers plan at the same moment; a second task with an existing open title is refused with the original named. */
  async function duplicateOpenTask(current: Run, title: unknown): Promise<string | null> {
    let tasks: View['tasks']['items'];
    try { tasks = (await fetchCollection(runtimeExecutable(), current.bridge.ready.run_id, current.sessionId, 'tasks', undefined, 8000)).tasks.items; }
    catch (error) { current.log('duplicate check skipped: ' + String((error as Error).message || error)); return null; }
    const existing = duplicateTask(title, tasks);
    if (!existing) return null;
    const agentName = (slot: number | null | undefined) => current.view?.agents.find(agent => agent.slot === slot)?.name || 'Agent ' + slot;
    current.log('worker refused duplicate task "' + String(title) + '" (existing task ' + existing.id + ')');
    return duplicateTaskReason(existing, agentName);
  }

  // ---- the live board ----------------------------------------------------------

  // Pi wraps widget lines at the terminal width, so a line wider than the
  // terminal costs a second row; the board sizes itself to what is there.
  function boardWidth(): number {
    const columns = Number(process.stdout.columns) || 100;
    return Math.max(40, Math.min(100, columns - 2));
  }

  function paintBoard(target: Run | null = run || lastFinished) {
    if (!target || !ctxRef?.hasUI) return;
    if (target.boardHidden || !target.view) { try { ctxRef.ui.setWidget('delm-board', undefined); } catch { /* no UI */ } return; }
    try { ctxRef.ui.setWidget('delm-board', renderBoard(target.view, boardWidth())); } catch { /* no UI */ }
  }

  function startBoard(ctx: ExtensionContext) {
    if (!run || !ctx.hasUI) return;
    const current = run;
    const observer = new BoardObserver(runtimeExecutable(), current.bridge.ready.run_id, current.sessionId);
    current.observer = observer;
    observer.start(view => {
      // The final snapshot lands after the run record is released; it still belongs on screen.
      if (run !== current && lastFinished !== current) return;
      current.view = view;
      paintBoard(current);
    }, message => current.log('board observer: ' + message));
  }

  function stopBoard(target: Run) {
    target.observer?.stop();
    target.observer = null;
  }

  async function showCollection(ctx: ExtensionContext, kind: 'tasks' | 'shared' | 'checks') {
    if (!run) { ctx.ui.notify('No DeLM run in this session.', 'info'); return; }
    let view: View;
    try { view = await fetchCollection(runtimeExecutable(), run.bridge.ready.run_id, run.sessionId, kind); }
    catch (error) { ctx.ui.notify('DeLM board detail unavailable: ' + String((error as Error).message || error), 'warning'); return; }
    const lines = renderCollection(view, kind, boardWidth());
    if (ctx.mode !== 'tui') {
      // Without the interactive UI the detail arrives as text.
      ctx.ui.notify(lines.filter(line => line && !/^Esc or q/.test(line)).slice(0, 40).join('\n'), 'info');
      return;
    }
    await ctx.ui.custom<void>((tui, _theme, _keys, done) => {
      let offset = 0;
      const component = {
        render(width: number) {
          const height = Math.max(5, Math.min(lines.length, 30));
          const page = lines.slice(offset, offset + height).map(line => ' ' + line.slice(0, Math.max(0, width - 2)));
          while (page.length < height) page.push('');
          return page;
        },
        invalidate() { /* nothing cached */ },
        handleInput(data: string) {
          if (data === '\x1b' || data === 'q' || data === '\r' || data === '\n') { done(); return; }
          if (data === '\x1b[B' || data === 'j') offset = Math.min(Math.max(0, lines.length - 5), offset + 1);
          if (data === '\x1b[A' || data === 'k') offset = Math.max(0, offset - 1);
          tui.requestRender();
        },
      };
      return component as never;
    }, {overlay: true, overlayOptions: {width: '80%', maxHeight: '80%', anchor: 'center' as never}});
  }

  // ---- shutdown and settlement -------------------------------------------------

  async function stopWorkers(current: Run | null = run): Promise<{id: string; status: string}[]> {
    if (!current) return [];
    await Promise.all(current.workers.map(async target => {
      if (target.stopPromise) return target.stopPromise;
      target.stopPromise = (async () => {
        if (target.process.running) {
          try { await target.process.abort(); } catch { /* The process may already be settling. */ }
          await target.process.stop();
          target.status = 'killed';
        } else if (target.status === 'running') {
          target.status = 'completed';
        }
      })();
      return target.stopPromise;
    }));
    return current.workers.map(target => ({id: target.agentId, status: target.status}));
  }

  async function settle(): Promise<void> {
    if (!run || run.finished || !run.finalization) return;
    if (run.settling) return run.settling;
    const current = run;
    const fence = {...current.finalization};
    current.settling = (async () => {
      try {
        // Delivery compares and copies whole workspaces; large projects take minutes, so these waits are long.
        const begun = await controlOn(current, 'begin_settle', {generation: fence.generation, revision: fence.revision}, 10 * 60 * 1000);
        if (current.stopRequested && (begun.finalization as {intent?: string})?.intent !== 'cancel') {
          const cancelled = await controlOn(current, 'cancel', {reason: 'User requested /delm-stop'});
          await applyActions((cancelled.actions as Action[]) || [], current);
          return;
        }
        const agents = await stopWorkers(current);
        const final = await controlOn(current, 'settle', {
          generation: fence.generation, revision: fence.revision, agents, background_tasks_stopped: true,
        }, 60 * 60 * 1000);
        await applyActions((final.actions as Action[]) || [], current);
      } catch (error) {
        if (run !== current || current.finished) return;   // abandon or a final action already reported
        if (current.bridgeExited) { current.failure = String((error as Error).message || error); await abandon(current, 'the DeLM runtime exited during delivery'); return; }
        const message = String((error as Error).message || error);
        // A cancel or an update replaced this finalization while it was being settled; the newer one is handled below.
        if (/obsolete/i.test(message) && (!current.finalization || current.finalization.generation !== fence.generation)) return;
        try { await controlOn(current, 'settlement_failed', {generation: fence.generation, revision: fence.revision}); } catch { /* Recovery. */ }
        // A failed delivery ends the run: the runtime keeps the private workspaces, and this session
        // releases the project through the same path as a lost runtime, saving what it can.
        current.failure = message;
        await abandon(current, 'delivery failed (' + message.replace(/\.$/, '') + ')');
      }
    })();
    try { await current.settling; } finally { current.settling = null; }
    // A newer finalization (for example /delm:stop during delivery) is settled in turn.
    if (run === current && !current.finished && current.finalization && current.finalization.generation !== fence.generation) await settle();
  }

  async function cleanup(current: Run | null = run) {
    if (!current || run !== current) return;
    // The board observer exits by itself once the run record is finished.
    for (const tickets of current.tickets) { try { await tickets.close(); } catch { /* Already closed. */ } }
    for (const target of current.workers) { if (target.process.running) void target.process.stop(1000).catch(() => undefined); }
    status(undefined);
    if (current.finished) { lastFinished = current; run = null; }
  }

  // ---- commands and input ----------------------------------------------------

  command('delm:run', 'Build in parallel with collaborating Pi agents and deliver their changes to this project', async (args, ctx) => {
      ctxRef = ctx;
      const task = (args || '').trim();
      if (run && !run.finished) {
        if (!task) { ctx.ui.notify('A DeLM run is already active. Send text to update it, or /delm:stop.', 'warning'); return; }
        if (run.finalization) { ctx.ui.notify('DeLM is finishing this run; the update was not sent. Start a new run once it ends.', 'warning'); return; }
        try { await forward(task); ctx.ui.notify('DeLM forwarded your update to the peers.', 'info'); }
        catch (error) { ctx.ui.notify('DeLM could not forward this update: ' + String((error as Error).message || error), 'error'); }
        return;
      }
      if (!task) { ctx.ui.notify('Enter a task after /delm:run.', 'warning'); return; }
      try {
        await launch(ctx, task);
        ctx.ui.notify('DeLM started two peers in private copies of this project. Send text here to update the task; /delm:stop cancels.', 'info');
      } catch (error) {
        ctx.ui.notify('DeLM could not start: ' + String((error as Error).message || error).replace(/^Error:\s*/, ''), 'error');
      }
  });

  command('delm:stop', 'Stop the active DeLM run and save unfinished changes', async (_args, ctx) => {
      ctxRef = ctx;
      if (!run || run.finished) { ctx.ui.notify('No DeLM run is active.', 'info'); return; }
      const current = run;
      current.stopRequested = true;
      try {
        const cancelled = await controlOn(current, 'cancel', {reason: 'User requested /delm-stop'});
        await applyActions((cancelled.actions as Action[]) || [], current);
      } catch (error) {
        if (current.bridgeExited || /no longer reachable/.test(String((error as Error).message))) { await abandon(current, 'the DeLM runtime is not reachable'); return; }
        ctx.ui.notify('DeLM could not stop cleanly: ' + String((error as Error).message || error), 'error');
      }
  });

  // /delm:status reopens the board, as in Claude Code; without a run it says so briefly.
  command('delm:status', 'Show the DeLM board for this session', async (_args, ctx) => {
      ctxRef = ctx;
      const target = run || lastFinished;
      if (!target) { ctx.ui.notify('No DeLM run in this session.', 'info'); return; }
      target.boardHidden = false;
      if (!ctx.hasUI) {
        ctx.ui.notify('DeLM ' + (target.view ? target.view.status : target.finished ? String(target.final?.status) : 'running')
          + (target.failure ? '. ' + target.failure : ''), 'info');
        return;
      }
      if (target.view) ctx.ui.setWidget('delm-board', renderBoard(target.view, boardWidth()));
      else ctx.ui.notify('DeLM board is loading.', 'info');
  });

  command('delm:board', 'DeLM board: show, hide, or open tasks, shared or checks', async (args, ctx) => {
      ctxRef = ctx;
      const word = (args || '').trim().toLowerCase();
      const target = run || lastFinished;
      if (!target) { ctx.ui.notify('No DeLM run in this session.', 'info'); return; }
      if (word === 'hide') { target.boardHidden = true; try { ctx.ui.setWidget('delm-board', undefined); } catch { /* no UI */ } return; }
      if (word === 'tasks' || word === 'shared' || word === 'checks') { await showCollection(ctx, word); return; }
      target.boardHidden = false;
      if (target.view) ctx.ui.setWidget('delm-board', renderBoard(target.view, boardWidth()));
      else ctx.ui.notify('DeLM board is loading.', 'info');
  });

  // Recovery of a run whose session ended before it finished: the bridge is
  // gone, so the controller's tokenless path confirms the host exited and
  // saves the unfinished changes, releasing the project for a new run.
  function canonical(path: string): string {
    try { return realpathSync(path); } catch { return resolve(path); }
  }

  function unfinishedRuns(project: string): {id: string; status: string}[] {
    const root = join(homedir(), 'Library', 'Application Support', 'DeLM', 'runs');
    if (!existsSync(root)) return [];
    // The controller records the canonical project path; the session's cwd may be a symlink to it.
    const wanted = new Set([project, resolve(project), canonical(project)]);
    const found: {id: string; status: string}[] = [];
    for (const id of readdirSync(root)) {
      try {
        const saved = JSON.parse(readFileSync(join(root, id, 'claude.json'), 'utf8')) as {host?: string; project?: string; finished?: boolean; status?: string};
        if (saved.host === 'pi' && !saved.finished && saved.project && (wanted.has(saved.project) || wanted.has(canonical(saved.project)))) {
          found.push({id, status: String(saved.status)});
        }
      } catch { /* not a run record */ }
    }
    return found;
  }

  command('delm:recover', 'Save the changes of an unfinished DeLM run that owns this project', async (_args, ctx) => {
      ctxRef = ctx;
      if (run && !run.finished) { ctx.ui.notify('A DeLM run is active here; use /delm:stop first.', 'warning'); return; }
      const stale = unfinishedRuns(ctx.cwd);
      if (!stale.length) { ctx.ui.notify('No unfinished DeLM run owns this project.', 'info'); return; }
      for (const item of stale) {
        // A run this session started is recovered with its own capability; older runs need the tokenless path.
        const own = lastFinished && lastFinished.bridge.ready.run_id === item.id ? lastFinished : null;
        const body = own
          ? {token: own.bridge.ready.token, session_id: own.sessionId, background_tasks_stopped: true,
            agents: own.workers.map(w => ({id: w.agentId, status: w.status === 'running' ? 'killed' : w.status}))}
          : {};
        const result = await recoverRun(item.id, body);
        if (result.ok) ctx.ui.notify('DeLM recovered: ' + result.message, 'info');
        else ctx.ui.notify('DeLM could not recover run ' + item.id.slice(0, 8) + ': ' + result.message
          + (own ? '' : ' If this Pi session started that run, quit Pi and run /delm:recover in a new session.'), 'error');
      }
  });

  pi.on('session_start', async (_event, ctx) => {
    if (!active()) return;
    ctxRef = ctx;
    if (process.env.DELM_PI_COMMAND) return;   // scripted hosts do not look at private storage
    const stale = unfinishedRuns(ctx.cwd);
    if (stale.length) {
      ctx.ui.notify('An unfinished DeLM run owns this project (' + stale.map(item => item.id.slice(0, 8) + ' ' + item.status).join(', ')
        + '). Run /delm:recover to save its changes before starting another.', 'warning');
    }
  });

  async function forward(text: string) {
    if (!run || run.finished) return;
    if (run.launching) throw new Error('DeLM is still starting its peers; send the update again in a moment.');
    const update = updateText(text);
    const current = run;
    // The new revision is registered first, so no step cites the old one; a peer still waiting at the
    // launch gate is then released and receives the update as a steer into its opening run.
    current.updating = true;
    let result;
    try { result = await control('update', {text: update}); } finally { current.updating = false; }
    current.revision = Number(result.revision) || current.revision;
    current.finalization = null;
    status('DeLM: revision ' + current.revision);
    if (current.launchGate) await current.launchGate.release();
    await applyActions((result.actions as Action[]) || [], current);
  }

  // While a run is working, ordinary text goes to the peers, not to the parent model.
  pi.on('input', async event => {
    if (!active()) return {action: 'continue'};
    if (!run || run.finished || event.source === 'extension') return {action: 'continue'};
    if (event.text.startsWith('/')) return {action: 'continue'};
    if (run.finalization) {
      notify('DeLM is finishing its run, so this message goes to this session rather than to the peers.', 'info');
      return {action: 'continue'};
    }
    try {
      await forward(event.text);
      notify('DeLM forwarded your update to the peers.');
    } catch (error) {
      notify('DeLM could not forward this update: ' + String((error as Error).message || error), 'error');
    }
    return {action: 'handled'};
  });

  pi.on('session_shutdown', async () => {
    if (lastFinished) stopBoard(lastFinished);
    if (run) stopBoard(run);
    if (!run || run.finished) return;
    const current = run;
    try { await controlOn(current, 'interrupt', {reason: 'Conversation ended'}, 2000); } catch { /* Recovery keeps the work. */ }
    // Pi waits for this handler without a bound; a stuck worker gets a short grace, then SIGKILL.
    await Promise.race([stopWorkers(current), new Promise(resolve => setTimeout(resolve, 6000))]);
    for (const target of current.workers) { if (target.process.alive) { try { target.process.child.kill('SIGKILL'); } catch { /* gone */ } } }
    current.finished = true;   // this session is over; the record stays for /delm:recover
    await cleanup(current);
  });
}
