// One-use coordination tickets cross from the worker process to the parent
// over a per-worker Unix socket. The worker's extension sends the exact tool
// and arguments it is about to call; the parent, which holds the control
// token, reserves a ticket with the controller and returns it together with
// the controller socket. The worker never learns the control token.
import {createServer, connect, type Server} from 'node:net';
import {chmodSync, mkdirSync, statSync} from 'node:fs';
import {join} from 'node:path';

export type TicketRequest = {call_id: string; tool: string; arguments: Record<string, unknown>};
export type TicketReply = {socket: string; ticket: string} | {error: string};
export type ReleaseRequest = {release: string};

const LIMIT = 256 * 1024;

export function bridgeRoot(uid = process.getuid?.() ?? 0): string {
  const root = join('/tmp', 'delm-' + uid, 'pi');
  mkdirSync(root, {recursive: true, mode: 0o700});
  const info = statSync(root);
  if (!info.isDirectory() || (info.mode & 0o077) !== 0) throw new Error('DeLM bridge storage is not private.');
  return root;
}

export class TicketServer {
  readonly path: string;
  private readonly server: Server;

  constructor(path: string, handler: (message: TicketRequest | ReleaseRequest) => Promise<TicketReply | {released: boolean}>,
    onOrphan: (ticket: string) => void = () => undefined) {
    this.path = path;
    this.server = createServer(socket => {
      let buffer = '';
      socket.setEncoding('utf8');
      socket.on('data', async chunk => {
        buffer += String(chunk);
        if (buffer.length > LIMIT) { socket.end(JSON.stringify({error: 'The coordination request is too large (over ' + LIMIT + ' bytes).'}) + '\n'); return; }
        const newline = buffer.indexOf('\n');
        if (newline < 0) return;
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        let message: TicketRequest | ReleaseRequest;
        try { message = JSON.parse(line); } catch { socket.end(JSON.stringify({error: 'Malformed bridge request.'}) + '\n'); return; }
        let reply: TicketReply | {released: boolean};
        try { reply = await handler(message); }
        catch (error) { reply = {error: String((error as Error).message || error)}; }
        if (socket.destroyed || !socket.writable) {
          // The worker stopped waiting; a ticket it never received must not stay reserved.
          if ('ticket' in reply && typeof reply.ticket === 'string') onOrphan(reply.ticket);
          return;
        }
        socket.end(JSON.stringify(reply) + '\n');
      });
      socket.on('error', () => { /* A worker that disconnects mid-request gets no ticket. */ });
    });
  }

  listen(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.server.once('error', reject);
      this.server.listen(this.path, () => {
        chmodSync(this.path, 0o600);
        resolve();
      });
    });
  }

  close(): Promise<void> {
    return new Promise(resolve => this.server.close(() => resolve()));
  }
}

// Worker side. Returns the controller socket and a ticket, or throws.
export function requestTicket(path: string, message: TicketRequest | ReleaseRequest, timeoutMs = 45000): Promise<TicketReply | {released: boolean}> {
  return new Promise((resolve, reject) => {
    const socket = connect(path);
    let buffer = '';
    let done = false;
    const finish = (error: Error | null, value?: TicketReply | {released: boolean}) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      socket.destroy();
      if (error) reject(error); else resolve(value!);
    };
    const timer = setTimeout(() => finish(new Error('DeLM did not issue a coordination ticket in time.')), timeoutMs);
    socket.setEncoding('utf8');
    socket.once('connect', () => socket.write(JSON.stringify(message) + '\n'));
    socket.on('data', chunk => {
      buffer += String(chunk);
      const newline = buffer.indexOf('\n');
      if (newline < 0) return;
      try { finish(null, JSON.parse(buffer.slice(0, newline))); } catch (error) { finish(error as Error); }
    });
    socket.once('error', error => finish(new Error('DeLM bridge is unavailable: ' + error.message)));
    socket.once('close', () => finish(new Error('DeLM bridge closed without a reply.')));
  });
}
