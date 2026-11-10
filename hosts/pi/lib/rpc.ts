// A Pi worker as a child process in RPC mode: newline-delimited JSON commands
// on stdin, responses and session events on stdout. Framing follows Pi's RPC
// contract: split stdout on LF only, correlate responses by id, and treat every
// other record as an event. Diagnostics stay on stderr.
import {spawn, type ChildProcess} from 'node:child_process';

export type RpcEvent = {type: string; [key: string]: unknown};
export type RpcResponse = {id?: string; type: 'response'; command: string; success: boolean; data?: unknown; error?: string};

const LIMIT = 8 * 1024 * 1024;

export class PiProcess {
  readonly child: ChildProcess;
  readonly pid: number;
  private sequence = 0;
  private buffer = '';
  private stderrTail = '';
  private readonly pending = new Map<string, {resolve: (value: unknown) => void; reject: (error: Error) => void}>();
  private readonly listeners = new Set<(event: RpcEvent) => void>();
  private exitCode: number | null = null;
  private exitSignal: NodeJS.Signals | null = null;
  private spawnError: Error | null = null;
  private discarding = false;
  readonly exited: Promise<{code: number | null; signal: NodeJS.Signals | null}>;

  constructor(command: string, args: string[], options: {cwd: string; env: NodeJS.ProcessEnv}) {
    this.child = spawn(command, args, {
      cwd: options.cwd, env: options.env, stdio: ['pipe', 'pipe', 'pipe'], shell: false,
    });
    // A missing executable reports asynchronously; a closed pipe reports on the
    // stream. Both are plain failures of this worker, never uncaught errors in
    // the parent session.
    let settled: ((value: {code: number | null; signal: NodeJS.Signals | null}) => void) | null = null;
    this.exited = new Promise(resolve => { settled = resolve; });
    const finish = (code: number | null, signal: NodeJS.Signals | null) => {
      if (this.exitCode !== null || this.exitSignal !== null || this.spawnError) return;
      this.exitCode = code; this.exitSignal = signal;
      const error = new Error('Pi worker exited' + (this.stderrTail.trim() ? ': ' + this.stderrTail.trim() : '.'));
      for (const waiter of this.pending.values()) waiter.reject(error);
      this.pending.clear();
      settled?.({code, signal});
    };
    // 'close' follows 'exit' once stdio drained, so the last events are delivered before
    // the exit is reported; a child whose pipes are held open by a grandchild is not waited for.
    this.child.once('exit', (code, signal) => {
      const timer = setTimeout(() => finish(code, signal), 250);
      this.child.once('close', () => { clearTimeout(timer); finish(code, signal); });
    });
    this.child.on('error', error => {
      this.spawnError = this.spawnError || error;
      for (const waiter of this.pending.values()) waiter.reject(error);
      this.pending.clear();
      settled?.({code: null, signal: null});
    });
    this.child.stdin?.on('error', error => {
      for (const waiter of this.pending.values()) waiter.reject(error);
      this.pending.clear();
    });
    this.pid = this.child.pid ?? 0;
    this.child.stdout?.setEncoding('utf8');
    this.child.stdout?.on('data', chunk => this.receive(String(chunk)));
    this.child.stdout?.on('error', () => { /* The exit handler reports the worker. */ });
    this.child.stderr?.setEncoding('utf8');
    this.child.stderr?.on('data', chunk => { this.stderrTail = (this.stderrTail + String(chunk)).slice(-8192); });
    this.child.stderr?.on('error', () => { /* Diagnostics only. */ });
  }

  /** The process has not reported its exit; a signal may already be on its way. */
  get alive(): boolean { return this.pid > 0 && !this.spawnError && this.exitCode === null && this.exitSignal === null; }
  /** Alive and not yet asked to stop. */
  get running(): boolean { return this.alive && !this.child.killed; }
  get stderr(): string { return this.stderrTail; }

  onEvent(listener: (event: RpcEvent) => void): () => void {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }

  private receive(chunk: string): void {
    if (this.discarding) {
      // The rest of an oversized record is dropped up to its line end.
      const end = chunk.indexOf('\n');
      if (end < 0) return;
      this.discarding = false;
      chunk = chunk.slice(end + 1);
    }
    this.buffer += chunk;
    if (this.buffer.length > LIMIT && !this.buffer.includes('\n')) {
      this.buffer = '';
      this.discarding = true;
      this.stderrTail = (this.stderrTail + '\n[delm] an event over ' + LIMIT + ' bytes was discarded').slice(-8192);
      return;
    }
    const lines = this.buffer.split('\n');
    this.buffer = lines.pop() ?? '';
    for (let line of lines) {
      if (line.endsWith('\r')) line = line.slice(0, -1);
      if (!line.trim()) continue;
      let record: RpcResponse | RpcEvent;
      try { record = JSON.parse(line); } catch { continue; }
      if (record.type === 'response') {
        const response = record as RpcResponse;
        const waiter = response.id === undefined ? undefined : this.pending.get(response.id);
        if (!waiter) continue;
        this.pending.delete(response.id!);
        if (response.success) waiter.resolve(response.data);
        else waiter.reject(new Error(response.error || 'Pi rejected the ' + response.command + ' command.'));
        continue;
      }
      for (const listener of [...this.listeners]) {
        try { listener(record as RpcEvent); } catch { /* A listener failure must not stop event delivery. */ }
      }
    }
  }

  send<T = unknown>(command: Record<string, unknown>, timeoutMs = 30000): Promise<T> {
    if (this.spawnError || !this.pid) return Promise.reject(new Error('Pi worker could not start' + (this.spawnError ? ': ' + this.spawnError.message : '.')));
    if (!this.running || !this.child.stdin?.writable) return Promise.reject(new Error('Pi worker is not running.'));
    const id = 'delm-' + (++this.sequence);
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error('Pi worker did not answer ' + String(command.type) + ' within ' + timeoutMs + ' ms.'));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: value => { clearTimeout(timer); resolve(value as T); },
        reject: error => { clearTimeout(timer); reject(error); },
      });
      this.child.stdin!.write(JSON.stringify({id, ...command}) + '\n', error => {
        if (error) { clearTimeout(timer); this.pending.delete(id); reject(error); }
      });
    });
  }

  prompt(message: string, streamingBehavior?: 'steer' | 'followUp', timeoutMs = 30000): Promise<{disposition: string}> {
    return this.send({type: 'prompt', message, ...(streamingBehavior ? {streamingBehavior} : {})}, timeoutMs);
  }

  steer(message: string): Promise<{disposition: string}> { return this.send({type: 'steer', message}); }
  followUp(message: string): Promise<{disposition: string}> { return this.send({type: 'follow_up', message}); }
  abort(): Promise<void> { return this.send({type: 'abort'}, 10000).then(() => undefined); }
  /** Answers a worker-side dialog request as cancelled; nobody is there to answer it. */
  cancelUiRequest(id: unknown): void {
    if (!this.running || !this.child.stdin?.writable) return;
    try { this.child.stdin.write(JSON.stringify({type: 'extension_ui_response', id, cancelled: true}) + '\n', () => undefined); } catch { /* exiting */ }
  }
  state(): Promise<Record<string, unknown>> { return this.send({type: 'get_state'}); }
  lastAssistantText(): Promise<string | null> {
    return this.send<{text: string | null}>({type: 'get_last_assistant_text'}).then(data => data?.text ?? null);
  }

  // Orderly shutdown: close stdin so Pi disposes its runtime, then escalate.
  async stop(graceMs = 5000): Promise<{code: number | null; signal: NodeJS.Signals | null}> {
    if (!this.running) return this.exited;
    try { this.child.stdin?.end(); } catch { /* Already closed. */ }
    const result = await Promise.race([this.exited, delay(graceMs).then(() => null)]);
    if (result) return result;
    this.child.kill('SIGTERM');
    const terminated = await Promise.race([this.exited, delay(graceMs).then(() => null)]);
    if (terminated) return terminated;
    this.child.kill('SIGKILL');
    return this.exited;
  }
}

function delay(ms: number): Promise<void> { return new Promise(resolve => setTimeout(resolve, ms)); }
