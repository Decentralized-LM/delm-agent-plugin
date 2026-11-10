// Loaded inside each DeLM worker process (a `pi --mode rpc` child of the
// parent session). It registers the DeLM coordination server and binds every
// coordination call to a one-use ticket obtained from the parent. In an
// ordinary session (no DELM_PI_WORKER) it does nothing.
import type {ExtensionAPI} from '@earendil-works/pi-coding-agent';
import {requestTicket} from '../lib/bridge.ts';
import {MCP_SERVER_NAME, coordinationTool, toolArguments} from '../lib/protocol.ts';

export default function (pi: ExtensionAPI) {
  const slot = process.env.DELM_PI_WORKER;
  const bridge = process.env.DELM_PI_BRIDGE;
  const runtime = process.env.DELM_PI_RUNTIME;
  if (!slot) return;
  // A second copy (the installed package plus a `-e` path) finds the server registered and stands down.
  let duplicate = false;
  if (!bridge || !runtime) {
    throw new Error('DeLM worker environment is incomplete: DELM_PI_BRIDGE and DELM_PI_RUNTIME are required.');
  }

  const tickets = new Map<string, string>();
  // Bash call ids this worker has made, newest last. A model sometimes cites
  // a shortened id; an unambiguous prefix is completed before the call is
  // bound, so the runtime still matches it to the observed command.
  const bashCalls: string[] = [];
  const otherCalls = new Set<string>();
  const MIN_PREFIX = 8;
  const complete = (value: unknown): unknown => {
    if (typeof value !== 'string' || !value || bashCalls.includes(value)) return value;
    // Short or sequential ids (call_1, call_10) must not be completed into a different call.
    if (value.length < MIN_PREFIX || otherCalls.has(value)) return value;
    const matches = bashCalls.filter(id => id.startsWith(value));
    return matches.length === 1 ? matches[0] : value;
  };
  const completeCommandIds = (tool: string, args: Record<string, unknown>) => {
    if (tool === 'delm_check_finish' && 'command_id' in args) args.command_id = complete(args.command_id);
    if (tool === 'delm_complete' && Array.isArray(args.checks)) args.checks = args.checks.map(complete);
  };

  pi.on('session_start', async () => {
    // The sidecar executes a board call only with a ticket the parent reserved.
    try {
      pi.registerMcpServer(MCP_SERVER_NAME, {
        command: runtime,
        args: ['pi', 'mcp'],
        exposure: 'direct',
        timeout: 120,
        description: 'DeLM shared task board for this run',
      } as never);
    } catch (error) {
      if (!/already registered/i.test(String((error as Error).message))) throw error;
      duplicate = true;
    }
  });

  pi.on('tool_call', async event => {
    if (duplicate) return undefined;
    if (event.toolName === 'bash') { bashCalls.push(event.toolCallId); if (bashCalls.length > 256) bashCalls.shift(); return undefined; }
    const tool = coordinationTool(event.toolName);
    if (!tool) { otherCalls.add(event.toolCallId); if (otherCalls.size > 1024) otherCalls.delete(otherCalls.values().next().value as string); return undefined; }
    const input = event.input as Record<string, unknown>;
    const arguments_ = toolArguments(input);
    completeCommandIds(tool, arguments_);
    let reply;
    try {
      reply = await requestTicket(bridge, {call_id: event.toolCallId, tool, arguments: arguments_});
    } catch (error) {
      return {block: true, reason: 'DeLM refused this coordination call: ' + String((error as Error).message || error)};
    }
    if ('error' in reply) return {block: true, reason: 'DeLM refused this coordination call: ' + reply.error};
    if (!('ticket' in reply)) return {block: true, reason: 'DeLM did not issue a coordination ticket.'};
    for (const key of Object.keys(input)) delete input[key];
    Object.assign(input, arguments_, {_delm: {socket: reply.socket, ticket: reply.ticket}});
    tickets.set(event.toolCallId, reply.ticket);
    return undefined;
  });

  pi.on('tool_result', async event => {
    if (duplicate) return undefined;
    const ticket = tickets.get(event.toolCallId);
    if (!ticket) return undefined;
    tickets.delete(event.toolCallId);
    // Tickets are single-use; release an unconsumed one so a refused native
    // call cannot be replayed later.
    try { await requestTicket(bridge, {release: ticket}); } catch { /* The parent revokes stale tickets at turn end. */ }
    return undefined;
  });
}
