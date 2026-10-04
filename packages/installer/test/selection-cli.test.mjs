import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {chmodSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {test} from 'node:test';
import {fileURLToPath} from 'node:url';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

function fixture(t, hosts, {configured = true, failingHost} = {}) {
  const directory = mkdtempSync(path.join(tmpdir(), 'delm CLI selection '));
  t.after(() => rmSync(directory, {recursive: true, force: true}));
  const packageDirectory = path.join(directory, 'package');
  const bin = path.join(directory, 'bin');
  const home = path.join(directory, 'home');
  const log = path.join(directory, 'native-calls.jsonl');
  mkdirSync(packageDirectory);
  mkdirSync(bin);
  mkdirSync(home);
  for (const name of ['bin', 'lib', 'package.json', 'release.json']) {
    cpSync(path.join(packageRoot, name), path.join(packageDirectory, name), {recursive: true});
  }
  const release = JSON.parse(readFileSync(path.join(packageDirectory, 'release.json'), 'utf8'));
  if (configured) release.repository = 'delm-fixture/selection';
  writeFileSync(path.join(packageDirectory, 'release.json'), JSON.stringify(release));

  for (const host of hosts) {
    const executable = path.join(bin, host);
    writeFileSync(executable, `#!${process.execPath}
import {appendFileSync, existsSync, writeFileSync} from 'node:fs';
const host = ${JSON.stringify(host)};
const args = process.argv.slice(2);
appendFileSync(process.env.DELM_SELECTION_LOG, JSON.stringify({host, args}) + '\\n');
const output = value => console.log(JSON.stringify(value));
const removed = process.env.DELM_SELECTION_STATE + '-' + host;
if (args[0] === '--version') {
  console.log(host === 'codex' ? 'codex-cli 0.157.0' : '2.1.289 (Claude Code)');
} else if (host === process.env.DELM_SELECTION_FAIL) {
  console.error('Fixture native manager unavailable');
  process.exitCode = 3;
} else if (host === 'codex') {
  if (args[1] === 'marketplace' && args[2] === 'list') {
    output({marketplaces: [{name: 'delm', marketplaceSource: {sourceType: 'git', source: 'https://github.com/delm-fixture/selection.git'}}]});
  } else if (args[1] === 'list') {
    output({installed: args.includes('delm-local') || existsSync(removed) ? [] : [{pluginId: 'delm@delm', installed: true, enabled: true, version: '0.3.0'}]});
  } else if (args[1] === 'remove') {
    writeFileSync(removed, 'removed');
    output({pluginId: 'delm@delm'});
  } else throw new Error('Unexpected fixture Codex command: ' + args.join(' '));
} else {
  if (args[1] === 'marketplace' && args[2] === 'list') {
    output([{name: 'delm', source: 'github', repo: 'delm-fixture/selection', ref: 'marketplace'}]);
  } else if (args[1] === 'list') {
    output(existsSync(removed) ? [] : [{id: 'delm@delm', scope: 'user', enabled: true, version: '0.3.0'}]);
  } else if (args[1] === 'uninstall') {
    writeFileSync(removed, 'removed');
    output({command: 'uninstall', outcome: 'ok', pluginId: 'delm@delm', scope: 'user'});
  } else throw new Error('Unexpected fixture Claude command: ' + args.join(' '));
}
`);
    // Native command names have no extension; mark their directory as ESM.
    writeFileSync(path.join(bin, 'package.json'), '{"type":"module"}');
    chmodSync(executable, 0o755);
  }

  const env = {
    PATH: bin, HOME: home, CODEX_HOME: path.join(home, '.codex'),
    CLAUDE_CONFIG_DIR: path.join(home, '.claude'),
    DELM_SELECTION_LOG: log, DELM_SELECTION_STATE: path.join(directory, 'state'),
    DELM_SELECTION_FAIL: failingHost ?? '',
  };
  return {
    bin,
    invoke: args => spawnSync(process.execPath, [path.join(packageDirectory, 'bin/delm-agent.mjs'), ...args], {
      cwd: directory, env, encoding: 'utf8', timeout: 15_000, input: '',
    }),
    calls: () => existsSync(log) ? readFileSync(log, 'utf8').trim().split('\n').map(line => JSON.parse(line)) : [],
    removed: host => existsSync(`${env.DELM_SELECTION_STATE}-${host}`),
  };
}

for (const host of ['codex', 'claude']) {
  test(`CLI selects the only detected host (${host}) and preserves single-host JSON`, t => {
    const native = fixture(t, [host]);
    const result = native.invoke(['status', '--json']);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, '');
    const state = JSON.parse(result.stdout);
    assert.equal(state.host, host);
    assert.equal(state.command, 'status');
    assert.equal(state.installed, true);
    assert.equal(state.version, '0.3.0');
    assert.equal(state.results, undefined);
    assert.ok(native.calls().some(call => call.args[0] === 'plugin'));
    assert.ok(native.calls().every(call => call.host === host));
  });
}

test('CLI rejects ambiguous piped and JSON requests before native management', t => {
  const native = fixture(t, ['codex', 'claude']);
  const plain = native.invoke(['status']);
  assert.equal(plain.status, 1);
  assert.match(plain.stderr, /--host/);
  assert.match(plain.stderr, /both/);
  const structured = native.invoke(['status', '--json']);
  assert.equal(structured.status, 1);
  assert.equal(structured.stdout, '');
  assert.equal(JSON.parse(structured.stderr).code, 'HOST_SELECTION_REQUIRED');
  assert.ok(native.calls().every(call => call.args.length === 1 && call.args[0] === '--version'));
});

test('CLI reports an actionable error when neither CLI is installed', t => {
  const native = fixture(t, []);
  const result = native.invoke(['status', '--json']);
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  const failure = JSON.parse(result.stderr);
  assert.equal(failure.code, 'MISSING_HOST');
  assert.match(failure.error, /Codex/);
  assert.match(failure.error, /Claude/);
  assert.match(failure.error, /install|PATH/i);
  assert.deepEqual(native.calls(), []);
});

test('CLI preserves the source release guard before any host probing', t => {
  const native = fixture(t, ['codex', 'claude'], {configured: false});
  const result = native.invoke(['status', '--json']);
  assert.equal(result.status, 1);
  assert.equal(JSON.parse(result.stderr).code, 'UNCONFIGURED_RELEASE');
  assert.deepEqual(native.calls(), []);
});

for (const failingHost of ['codex', 'claude']) {
  test(`CLI both-host status preserves success when ${failingHost} fails`, t => {
    const native = fixture(t, ['codex', 'claude'], {failingHost});
    const result = native.invoke(['status', '--host', 'both', '--json']);
    assert.equal(result.status, 1);
    assert.equal(result.stderr, '');
    const outcome = JSON.parse(result.stdout);
    assert.equal(outcome.command, 'status');
    assert.equal(outcome.success, false);
    assert.equal(outcome.results.length, 1);
    assert.equal(outcome.results[0].host, failingHost === 'codex' ? 'claude' : 'codex');
    assert.equal(outcome.results[0].installed, true);
    assert.equal(outcome.errors.length, 1);
    assert.equal(outcome.errors[0].host, failingHost);
    assert.equal(outcome.errors[0].code, 'NATIVE_COMMAND_FAILED');
    assert.match(outcome.errors[0].error, /Fixture native manager unavailable/);
  });
}

test('CLI explicit executable selection does not fall back to another installed host', t => {
  const native = fixture(t, ['codex', 'claude']);
  const result = native.invoke(['status', '--claude', path.join(native.bin, 'claude'), '--json']);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).host, 'claude');
  assert.ok(native.calls().every(call => call.host === 'claude'));
});

test('CLI both-host removal retains truthful partial success after one native failure', {
  skip: process.platform !== 'darwin' ? 'Native mutations currently support macOS only.' : false,
}, t => {
  const native = fixture(t, ['codex', 'claude'], {failingHost: 'claude'});
  const result = native.invoke(['remove', '--host', 'both', '--json']);
  assert.equal(result.status, 1);
  assert.equal(result.stderr, '');
  const outcome = JSON.parse(result.stdout);
  assert.equal(outcome.success, false);
  assert.equal(outcome.results.length, 1);
  assert.equal(outcome.results[0].host, 'codex');
  assert.equal(outcome.results[0].changed, true);
  assert.equal(outcome.results[0].installed, false);
  assert.equal(outcome.errors.length, 1);
  assert.equal(outcome.errors[0].host, 'claude');
  assert.equal(native.removed('codex'), true);
  assert.equal(native.removed('claude'), false);
  assert.equal(native.calls().filter(call => call.host === 'codex' && call.args[1] === 'remove').length, 1);
});
