// The DeLM controller as seen from the parent Pi session: `delm pi serve` is
// started once per run, prints the ready contract, then streams actions.
// Control requests go to its private Unix socket with the run's token. The
// token never reaches a worker process.
import {spawn, type ChildProcess} from 'node:child_process';
import {connect} from 'node:net';
import {decodeLines, validateReady, type Ready} from './protocol.ts';

export type Action = {type: string; id?: string; [key: string]: unknown};

const LIMIT = 2 * 1024 * 1024;

export async function request(ready: Pick<Ready, 'socket' | 'token'>, op: string, fields: Record<string, unknown> = {},
  timeoutMs = 120000): Promise<Record<string, unknown>> {
  const body = JSON.stringify({op, token: ready.token, ...fields});
  if (body.length >= LIMIT) throw new Error('DeLM control request is too large.');
  return new Promise((resolve, reject) => {
    const socket = connect(ready.socket);
    let buffer = '';
    let settled = false;
    const finish = (error: Error | null, value?: Record<string, unknown>) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      if (error) reject(error); else resolve(value!);
    };
    const timer = setTimeout(() => finish(new Error('DeLM did not respond to ' + op + '.')), timeoutMs);
    socket.setEncoding('utf8');
    socket.once('connect', () => socket.write(body + '\n'));
    socket.on('data', chunk => {
      buffer += String(chunk);
      const newline = buffer.indexOf('\n');
      if (newline < 0) return;
      let reply: {ok?: boolean; result?: Record<string, unknown>; error?: string};
      try { reply = JSON.parse(buffer.slice(0, newline)); } catch (error) { return finish(error as Error); }
      if (!reply || reply.ok !== true) return finish(new Error(reply?.error || 'DeLM did not return a successful response.'));
      finish(null, reply.result ?? {});
    });
    socket.once('error', error => finish(new Error('DeLM is no longer reachable: ' + error.message)));
    socket.once('close', () => finish(new Error('DeLM closed the control connection without a result.')));
  });
}

export type Bridge = {
  ready: Ready;
  process: ChildProcess;
  stderr: () => string;
  /** Resolves when the serve process exits; actions printed meanwhile reach `onAction`. */
  exited: Promise<number | null>;
};

export function startBridge(executable: string, start: {project: string; session_id: string; task: string;
  host_version?: string | null; package_root?: string | null}, onAction: (actions: Action[]) => void): Promise<Bridge> {
  return new Promise((resolve, reject) => {
    const child = spawn(executable, ['pi', 'serve'], {stdio: ['pipe', 'pipe', 'pipe'], shell: false});
    let buffer = '';
    let stderr = '';
    let ready: Ready | null = null;
    child.stdout!.setEncoding('utf8');
    child.stderr!.setEncoding('utf8');
    child.stderr!.on('data', chunk => { stderr = (stderr + String(chunk)).slice(-8192); });
    const exited = new Promise<number | null>(settle => child.once('exit', code => settle(code)));
    child.once('error', error => reject(error));
    child.stdin!.on('error', error => reject(new Error('DeLM runtime closed its input: ' + error.message + (stderr.trim() ? ' (' + stderr.trim() + ')' : ''))));
    child.stdout!.on('data', chunk => {
      const decoded = decodeLines(buffer, String(chunk));
      buffer = decoded.rest;
      if (decoded.skipped.length) stderr = (stderr + '\n[stdout] ' + decoded.skipped.join('\n[stdout] ')).slice(-8192);
      for (const event of decoded.events) {
        if (!ready) {
          try { ready = validateReady(event); } catch (error) { reject(error as Error); return; }
          resolve({ready, process: child, stderr: () => stderr, exited});
          continue;
        }
        if (event.type === 'action') {
          const result = event.result as {actions?: Action[]} | undefined;
          onAction(result?.actions || []);
        }
      }
    });
    exited.then(() => {
      if (!ready) reject(new Error(stderr.trim() || 'DeLM could not prepare its private workspaces.'));
    });
    child.stdin!.write(JSON.stringify(start) + '\n');
  });
}
