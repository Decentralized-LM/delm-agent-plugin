#!/usr/bin/env node
// A deterministic stand-in for `pi --mode rpc` as a DeLM worker. No model.
//
// Worker 1 runs one real bash command in its working directory, then declares
// completion through the real DeLM sidecar with a ticket from the parent's
// bridge, exactly as the worker extension would. Worker 2 declares that it is
// waiting on worker 1 on every prompt. The scenario is selected by
// DELM_PI_WORKER; the controller, sidecar and delivery are production code.
import {spawn} from 'node:child_process';
import {appendFileSync, existsSync, writeFileSync} from 'node:fs';
import {connect} from 'node:net';
import {createInterface} from 'node:readline';

const slot = Number(process.env.DELM_PI_WORKER || '0');
const bridge = process.env.DELM_PI_BRIDGE;
const runtime = process.env.DELM_PI_RUNTIME;
const cwd = process.cwd();
const scenario = process.env.FAKE_PI_SCENARIO || 'deliver';

const emit = record => process.stdout.write(JSON.stringify(record) + '\n');
const respond = (id, command, data) => emit({id, type: 'response', command, success: true, ...(data === undefined ? {} : {data})});

let streaming = false;
let lastText = null;
let calls = 0;

function bridgeRequest(message) {
  return new Promise((resolve, reject) => {
    const socket = connect(bridge);
    let buffer = '';
    socket.setEncoding('utf8');
    socket.once('connect', () => socket.write(JSON.stringify(message) + '\n'));
    socket.on('data', chunk => {
      buffer += chunk;
      const newline = buffer.indexOf('\n');
      if (newline >= 0) { socket.destroy(); resolve(JSON.parse(buffer.slice(0, newline))); }
    });
    socket.once('error', reject);
  });
}

// One JSON-RPC conversation with the real sidecar: initialize, then the call.
function sidecarCall(name, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(runtime, ['pi', 'mcp'], {stdio: ['pipe', 'pipe', 'inherit']});
    const lines = createInterface({input: child.stdout});
    const replies = new Map();
    lines.on('line', line => {
      let message;
      try { message = JSON.parse(line); } catch { return; }
      replies.get(message.id)?.(message);
    });
    const send = (id, method, params) => new Promise(done => {
      replies.set(id, done);
      child.stdin.write(JSON.stringify({jsonrpc: '2.0', id, method, params}) + '\n');
    });
    (async () => {
      await send(1, 'initialize', {protocolVersion: '2025-06-18'});
      const reply = await send(2, 'tools/call', {name, arguments: args});
      child.stdin.end();
      const text = reply.result?.content?.[0]?.text ?? '';
      let parsed = null;
      try { parsed = JSON.parse(text); } catch { /* error text */ }
      if (reply.result?.isError) reject(new Error('sidecar refused ' + name + ': ' + text));
      else resolve(parsed);
    })().catch(reject);
  });
}

async function coordinate(tool, args) {
  const callId = 'call-' + slot + '-' + (++calls);
  const ticket = await bridgeRequest({call_id: callId, tool, arguments: args});
  if (ticket.error) throw new Error('bridge refused: ' + ticket.error);
  emit({type: 'tool_execution_start', toolCallId: callId, toolName: 'mcp__delm__' + tool, args});
  let result;
  try {
    result = await sidecarCall(tool, {...args, _delm: {socket: ticket.socket, ticket: ticket.ticket}});
  } finally {
    await bridgeRequest({release: ticket.ticket}).catch(() => {});
  }
  emit({type: 'tool_execution_end', toolCallId: callId, toolName: 'mcp__delm__' + tool, result: {content: [{type: 'text', text: JSON.stringify(result)}]}, isError: false});
  return result;
}

function bash(command) {
  return new Promise(resolve => {
    const callId = 'call-' + slot + '-' + (++calls);
    emit({type: 'tool_execution_start', toolCallId: callId, toolName: 'bash', args: {command}});
    const child = spawn('/bin/sh', ['-c', command], {cwd, stdio: ['ignore', 'pipe', 'pipe']});
    let output = '';
    child.stdout.on('data', chunk => { output += chunk; });
    child.stderr.on('data', chunk => { output += chunk; });
    child.on('close', code => {
      emit({type: 'tool_execution_end', toolCallId: callId, toolName: 'bash',
        result: {content: [{type: 'text', text: output || '(no output)'}], structuredContent: {output, truncated: false, exit_code: code, wall_time_seconds: 0}},
        isError: code !== 0});
      resolve({callId, code});
    });
  });
}

let onAbort = null;

async function run(prompt) {
  streaming = true;
  emit({type: 'agent_start'});
  emit({type: 'turn_start'});
  if (scenario === 'idle') {
    // A worker that is thinking for a long time; the abort command settles it.
    await new Promise(resolve => { onAbort = resolve; });
    return;
  }
  try {
    if (slot === 1 && scenario === 'deliver') {
      // Planning takes a moment; peer 2 must not be prompted before this task exists.
      if (process.env.FAKE_PI_MARK) await new Promise(resolve => setTimeout(resolve, 2000));
      await coordinate('delm_task_create', {idempotency_key: 'task-1-' + calls, title: 'Write hello.txt', description: 'Create hello.txt with a greeting.', kind: 'implementation'});
      // Stay active until peer 2 has recorded its duplicate attempt (bounded, so a broken gate still ends the test).
      if (process.env.FAKE_PI_MARK) {
        const until = Date.now() + 30000;
        while (!existsSync(process.env.FAKE_PI_MARK) && Date.now() < until) await new Promise(resolve => setTimeout(resolve, 100));
      }
      const check = await bash("printf 'hello from delm\\n' > hello.txt && cat hello.txt");
      // A transient provider error that Pi retries: the parent must not stop the run for it.
      emit({type: 'message_end', message: {role: 'assistant', stopReason: 'error', errorMessage: '529 {"error":{"type":"overloaded_error","message":"Overloaded"}}'}});
      emit({type: 'agent_end', messages: [], willRetry: true});
      // Pi starts another model loop within the same run (retry, or draining a steer); the turn must survive it.
      emit({type: 'agent_start'});
      emit({type: 'turn_start'});
      emit({type: 'message_end', message: {role: 'assistant', stopReason: 'stop'}});
      emit({type: 'agent_end', messages: [], willRetry: false});
      let continued = 'continued_turn=ok';
      try { await coordinate('delm_status', {}); } catch (error) { continued = 'continued_turn=' + error.message; }
      if (process.env.FAKE_PI_MARK) appendFileSync(process.env.FAKE_PI_MARK, continued + '\n');
      await coordinate('delm_complete', {
        idempotency_key: 'complete-1-' + calls, expected_revision: 1, outcome: 'complete',
        summary: 'Created hello.txt with a greeting; verified by printing it.', checks: [check.callId], shared_checks: [], artifacts: [],
      });
      lastText = 'Created hello.txt in the project and verified its contents.';
    } else {
      if (scenario === 'deliver' && process.env.FAKE_PI_MARK) {
        // Peer 1 shares its task first; the same title from peer 2 must be refused by the parent with the original named.
        const board = await coordinate('delm_status', {});
        const seen = board?.board?.tasks?.length ?? 0;
        let refusal = 'not refused';
        try { await coordinate('delm_task_create', {idempotency_key: 'task-2-' + calls, title: 'write hello.txt', description: 'Duplicate plan.', kind: 'implementation'}); }
        catch (error) { refusal = error.message; }
        writeFileSync(process.env.FAKE_PI_MARK, 'tasks_at_start=' + seen + '\nprompt=' + prompt.replace(/\n/g, ' ') + '\n' + refusal + '\n');
      }
      await coordinate('delm_complete', {
        idempotency_key: 'waiting-' + slot + '-' + calls, expected_revision: 1, outcome: 'waiting',
        summary: 'Nothing independent remains; waiting for peer 1.', dependency: 'worker:1', checks: [], shared_checks: [], artifacts: [],
      });
      lastText = 'Waiting for peer 1.';
    }
  } catch (error) {
    process.stderr.write('fake-pi worker ' + slot + ': ' + error.message + '\n');
    lastText = 'Failed: ' + error.message;
  }
  streaming = false;
  emit({type: 'turn_end', message: {role: 'assistant', content: [{type: 'text', text: lastText}]}, toolResults: []});
  emit({type: 'agent_end', messages: [], willRetry: false});
  emit({type: 'agent_settled', aborted: false});
}

const input = createInterface({input: process.stdin});
input.on('line', line => {
  let command;
  try { command = JSON.parse(line); } catch { return; }
  switch (command.type) {
    case 'prompt':
      respond(command.id, 'prompt', {disposition: 'started'});
      void run(command.message);
      break;
    case 'steer':
    case 'follow_up':
      respond(command.id, command.type, {disposition: 'queued'});
      break;
    case 'abort':
      respond(command.id, 'abort');
      if (streaming) { streaming = false; emit({type: 'agent_settled', aborted: true}); }
      onAbort?.();
      break;
    case 'get_last_assistant_text':
      respond(command.id, 'get_last_assistant_text', {text: lastText});
      break;
    case 'get_state':
      respond(command.id, 'get_state', {isStreaming: streaming, sessionId: 'fake-' + slot});
      break;
    default:
      emit({id: command.id, type: 'response', command: command.type, success: false, error: 'unsupported in fake-pi'});
  }
});
input.on('close', () => process.exit(0));
