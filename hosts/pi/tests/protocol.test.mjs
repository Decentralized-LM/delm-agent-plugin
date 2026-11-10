import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';
import {commandOutcome, coordinationTool, decodeLines, duplicateTask, duplicateTaskReason, explainModelError, launchPrompt, toolArguments, turnReason, updateText, validateReady}
  from '../lib/protocol.ts';

const ready = {
  type: 'ready', run_id: 'run', session_id: 'session', socket: '/tmp/delm-1/x.sock', token: 'a'.repeat(32),
  executable: '/usr/local/bin/delm', revision: 1, workers: [{slot: 1, cwd: '/w/1'}, {slot: 2, cwd: '/w/2'}],
};

test('ready contract requires two distinct absolute worker directories', () => {
  assert.equal(validateReady(ready).run_id, 'run');
  assert.throws(() => validateReady({...ready, workers: [ready.workers[0], ready.workers[0]]}));
  assert.throws(() => validateReady({...ready, token: 'short'}));
  assert.throws(() => validateReady({...ready, revision: 0}));
  assert.throws(() => validateReady({...ready, type: 'action'}));
});

test('lines decode across chunks, tolerate CRLF, and skip stray text without losing records', () => {
  const first = decodeLines('', '{"a":1}\r\n{"b":');
  assert.deepEqual(first.events, [{a: 1}]);
  const second = decodeLines(first.rest, '2}\n');
  assert.deepEqual(second.events, [{b: 2}]);
  assert.equal(second.rest, '');
  const noisy = decodeLines('', 'warning: something odd\n{"c":3}\n[1]\n{"d":4}\n');
  assert.deepEqual(noisy.events, [{c: 3}, {d: 4}]);
  assert.deepEqual(noisy.skipped, ['warning: something odd', '[1]']);
  const huge = decodeLines('', 'x'.repeat(2 * 1024 * 1024 + 1));
  assert.equal(huge.rest, '');
  assert.equal(huge.skipped.length, 1);
});

test('a forged transport binding is dropped from model arguments', () => {
  assert.deepEqual(toolArguments({summary: 'x', _delm: {socket: '/evil', ticket: 't'}}), {summary: 'x'});
  assert.equal(coordinationTool('mcp__delm__delm_publish'), 'delm_publish');
  assert.equal(coordinationTool('bash'), null);
});

test('bash evidence takes the exit code from structured content only', () => {
  const textOnly = commandOutcome({result: {content: [{type: 'text', text: 'boom\nCommand exited with code 2'}]}, isError: true});
  assert.equal(textOnly.exit_code, null, 'a status line in text is not an observed exit code');
  const ok = commandOutcome({result: {content: [{type: 'text', text: 'done'}], structuredContent: {exit_code: 0}}, isError: false});
  assert.equal(ok.exit_code, 0);
  assert.equal(ok.is_error, false);
  const failed = commandOutcome({result: {content: [{type: 'text', text: 'boom\nCommand exited with code 2'}],
    structuredContent: {exit_code: 2}}, isError: true});
  assert.equal(failed.exit_code, 2);
  assert.equal(failed.is_error, true);
  const aborted = commandOutcome({result: {content: [{type: 'text', text: 'Command aborted.'}]}, isError: true});
  assert.equal(aborted.exit_code, null);
  assert.equal(aborted.interrupted, true);
  const timedOut = commandOutcome({result: {content: [{type: 'text', text: 'Command timed out after 30 seconds'}]}, isError: true});
  assert.equal(timedOut.timed_out, true);
});

test('turn reasons distinguish completion, abort and process loss', () => {
  assert.equal(turnReason({aborted: false}, false), 'completed');
  assert.equal(turnReason({aborted: true}, false), 'interrupted');
  assert.equal(turnReason({aborted: false}, true), 'failed');
});

test('updates are bounded and the launch prompt carries the request', () => {
  assert.throws(() => updateText('   '));
  assert.throws(() => updateText('x'.repeat(128 * 1024 + 1)));
  assert.match(updateText('fix it', ['see README']), /Additional context:\nsee README$/);
  assert.match(launchPrompt('Build a thing', 2), /peer 2 of 2[\s\S]*User request:\nBuild a thing$/);
  assert.match(launchPrompt('Build a thing', 1), /share the initial task breakdown promptly/);
  assert.match(launchPrompt('Build a thing', 2), /already lists your peer's initial task breakdown/);
  assert.match(launchPrompt('Build a thing', 2, false), /has not shared a task breakdown yet/);
});

test('model refusals are explained with the fix, not the raw API body', () => {
  const quota = explainModelError('400 {"type":"error","error":{"type":"invalid_request_error","message":"You\'re out of extra usage. Add more at claude.ai/settings/usage and keep going."},"request_id":"req_1"}');
  assert.match(quota, /no extra usage left/);
  assert.match(quota, /API key/);
  assert.match(explainModelError('401 {"error":{"message":"invalid x-api-key"}}'), /rejected the login/);
  assert.match(explainModelError('429 rate_limit_error: too many requests'), /rate limiting/);
  assert.match(explainModelError('something odd happened'), /provider returned: something odd happened/);
});

test('a second task with an open title is detected regardless of case, spacing or punctuation', () => {
  const tasks = [
    {id: 3, task_number: 1, title: 'calc/ops.py + tests/test_ops.py', state: 'claimed', owner: 2},
    {id: 4, task_number: 2, title: 'README', state: 'done', owner: 1},
  ];
  assert.equal(duplicateTask('Calc ops.py, tests test_ops.py', tasks)?.id, 3);
  assert.equal(duplicateTask('readme', tasks), null, 'finished tasks never match');
  const symbols = [{id: 9, title: 'C++ bindings', state: 'claimed', owner: 1}, {id: 10, title: 'Fix #12', state: 'available', owner: null}];
  assert.equal(duplicateTask('C bindings', symbols), null, 'C and C++ are different work');
  assert.equal(duplicateTask('c++  bindings', symbols)?.id, 9);
  assert.equal(duplicateTask('Fix 12', symbols), null, 'an issue number is not plain text');
  assert.equal(duplicateTask('fix: #12', symbols)?.id, 10);
  assert.equal(duplicateTask('Fix#12', symbols)?.id, 10, 'spacing around a symbol does not matter');
  const glued = [{id: 11, title: 'calc/ops.py + tests', state: 'available', owner: null}, {id: 12, title: 'tests & docs', state: 'available', owner: null}];
  assert.equal(duplicateTask('calc/ops.py+tests', glued)?.id, 11);
  assert.equal(duplicateTask('tests&docs', glued)?.id, 12);
  assert.equal(duplicateTask('calc/cli.py', tasks), null);
  assert.equal(duplicateTask('', tasks), null);
  const reason = duplicateTaskReason(tasks[0], slot => 'Agent ' + slot);
  assert.match(reason, /^A task with this title already exists: #1 "calc\/ops.py \+ tests\/test_ops.py" \(claimed by Agent 2, task_id 3\)/);
  assert.match(duplicateTaskReason({...tasks[0], state: 'available', owner: null}, () => 'x'), /\(available, task_id 3\)/);
});

test('the development copy of the worker policy matches the shared policy the build stages', () => {
  const here = new URL('.', import.meta.url).pathname;
  assert.equal(readFileSync(here + '../worker.md', 'utf8'), readFileSync(here + '../../../plugin/worker.md', 'utf8'),
    'hosts/pi/worker.md drifted from plugin/worker.md; copy plugin/worker.md over it');
});

test('billing refusals and real 404s are account errors; a 404 inside a 5xx body is not', () => {
  assert.match(explainModelError('400 {"error":{"message":"Your credit balance is too low to access the Anthropic API."}}'), /no credit/);
  assert.match(explainModelError('404 {"error":{"type":"not_found_error","message":"model: nope"}}'), /does not offer/);
  assert.doesNotMatch(explainModelError('500 Internal server error, 404 tokens were processed'), /does not offer/);
  assert.match(explainModelError('429 {"error":{"type":"rate_limit_error","message":"You have reached your specified API usage limits. You will regain access on 2026-12-01 at 00:00 UTC."}}'),
    /monthly usage limit.*Anthropic Console/);
});
