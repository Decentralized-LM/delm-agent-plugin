import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {chmod, mkdir, mkdtemp, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {PassThrough} from 'node:stream';
import {test} from 'node:test';
import {discoverHosts, manageHosts, promptForHosts, selectHosts, validateHostOptions} from '../lib/hosts.mjs';
import {InstallerError} from '../lib/installer.mjs';

test('discovery finds executable files and symlinks in PATH without executing them', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'delm host discovery '));
  try {
    await mkdir(path.join(directory, 'first'));
    await mkdir(path.join(directory, 'second'));
    await mkdir(path.join(directory, 'first', 'codex'));
    await writeFile(path.join(directory, 'first', 'claude'), 'not executable');
    const executable = path.join(directory, 'host with spaces');
    await writeFile(executable, '#!/bin/sh\nexit 91\n');
    await chmod(executable, 0o755);
    await symlink(executable, path.join(directory, 'second', 'codex'));
    await symlink(executable, path.join(directory, 'second', 'claude'));
    assert.deepEqual(await discoverHosts({env: {PATH: `first${path.delimiter}second`}, cwd: directory}), {
      codex: path.join(directory, 'second', 'codex'),
      claude: path.join(directory, 'second', 'claude'),
    });
    assert.deepEqual(await discoverHosts({env: {}, cwd: directory}), {});
    assert.deepEqual(await discoverHosts({env: {PATH: path.join(directory, 'missing')}}), {});
  } finally {
    await rm(directory, {recursive: true, force: true});
  }
});

test('Windows status discovery recognizes PATHEXT without running scripts', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'delm windows discovery '));
  try {
    await writeFile(path.join(directory, 'codex.EXE'), 'fixture');
    await writeFile(path.join(directory, 'claude.CMD'), 'fixture');
    assert.deepEqual(await discoverHosts({platform: 'win32', env: {PATH: directory, PATHEXT: '.EXE;.CMD'}}), {
      codex: path.join(directory, 'codex.EXE'), claude: path.join(directory, 'claude.CMD'),
    });
  } finally {
    await rm(directory, {recursive: true, force: true});
  }
});

test('host and executable options have explicit and unambiguous precedence', () => {
  assert.equal(validateHostOptions(), null);
  assert.deepEqual(validateHostOptions({host: 'codex'}), ['codex']);
  assert.deepEqual(validateHostOptions({host: 'claude'}), ['claude']);
  assert.deepEqual(validateHostOptions({host: 'both'}), ['codex', 'claude']);
  assert.deepEqual(validateHostOptions({codex: './custom codex'}), ['codex']);
  assert.deepEqual(validateHostOptions({claude: './custom claude'}), ['claude']);
  assert.deepEqual(validateHostOptions({codex: './a', claude: './b'}), ['codex', 'claude']);
  assert.deepEqual(validateHostOptions({host: 'both', claude: './b'}), ['codex', 'claude']);
  for (const options of [
    {host: ''}, {host: 'other'}, {host: 'Codex'}, {codex: ''}, {claude: '  '},
    {codex: false}, {host: 'codex', claude: 'claude'}, {host: 'claude', codex: 'codex'},
  ]) assert.throws(() => validateHostOptions(options), {code: 'USAGE'});
});

test('a sole available CLI is automatically selected in all output modes', async () => {
  for (const host of ['codex', 'claude']) {
    for (const json of [true, false]) {
      assert.deepEqual(await selectHosts({json}, {
        interactive: false,
        discover: async () => ({[host]: `/bin/${host}`}),
        prompt: () => assert.fail('One installed host must not prompt'),
      }), [{host, [host]: `/bin/${host}`}]);
    }
  }
});

test('explicit hosts and executable options skip both discovery and prompting', async () => {
  const dependencies = {
    discover: () => assert.fail('Explicit choice should not discover'),
    prompt: () => assert.fail('Explicit choice should not prompt'),
  };
  assert.deepEqual(await selectHosts({host: 'both', claude: '/custom claude'}, dependencies), [
    {host: 'codex', codex: 'codex'}, {host: 'claude', claude: '/custom claude'},
  ]);
  assert.deepEqual(await selectHosts({codex: '/custom codex'}, dependencies), [{host: 'codex', codex: '/custom codex'}]);
  assert.deepEqual(await selectHosts({claude: '/custom claude'}, dependencies), [{host: 'claude', claude: '/custom claude'}]);
});

test('both detected hosts require a selection in JSON or noninteractive mode', async () => {
  for (const [json, interactive] of [[true, true], [true, false], [false, false]]) {
    await assert.rejects(selectHosts({json}, {
      interactive,
      discover: async () => ({codex: '/a', claude: '/b'}),
      prompt: () => assert.fail('Noninteractive mode must not prompt'),
    }), error => error.code === 'HOST_SELECTION_REQUIRED' && error.message.includes('--host both'));
  }
});

test('interactive selection passes exactly the chosen host and its PATH executable', async () => {
  for (const chosen of [['codex'], ['claude'], ['codex', 'claude']]) {
    let prompts = 0;
    assert.deepEqual(await selectHosts({}, {
      interactive: true,
      discover: async () => ({codex: '/one', claude: '/two'}),
      prompt: async () => { prompts++; return chosen; },
    }), chosen.map(host => ({host, [host]: host === 'codex' ? '/one' : '/two'})));
    assert.equal(prompts, 1);
  }
});

test('no available host gives setup guidance without prompting', async () => {
  await assert.rejects(selectHosts({}, {
    discover: async () => ({}),
    prompt: () => assert.fail('Cannot select an unavailable host'),
  }), error => error.code === 'MISSING_HOST' && error.message.includes('--claude PATH') && error.message.includes('--codex PATH'));
});

function promptFixture() {
  const input = new PassThrough();
  const output = new PassThrough();
  const signals = new EventEmitter();
  let text = '';
  output.on('data', data => { text += data; });
  return {input, output, signals, text: () => text};
}

test('selection prompt has no default, retries invalid input, and cleans signal listeners', async () => {
  const streams = promptFixture();
  const existing = () => {};
  streams.signals.on('SIGINT', existing);
  const selected = promptForHosts(streams);
  streams.input.write('\nother\n2\n');
  assert.deepEqual(await selected, ['claude']);
  assert.match(streams.text(), /1\. Codex[\s\S]*2\. Claude Code[\s\S]*3\. Both/);
  assert.equal((streams.text().match(/Enter 1, 2, or 3/g) ?? []).length, 2);
  assert.deepEqual(streams.signals.listeners('SIGINT'), [existing]);
  assert.equal(streams.signals.listenerCount('SIGTERM'), 0);
  assert.equal(streams.input.listenerCount('data'), 0);
});

test('EOF and process signals cancel the prompt without any host operation', async () => {
  for (const action of ['EOF', 'SIGINT', 'SIGTERM']) {
    const streams = promptFixture();
    let calls = 0;
    const operation = manageHosts('install', {}, {
      validate: () => {},
      selection: {
        interactive: true,
        discover: async () => ({codex: '/a', claude: '/b'}),
        prompt: () => promptForHosts(streams),
      },
      run: async () => { calls++; },
    });
    const rejected = assert.rejects(operation, {code: 'CANCELLED'});
    await new Promise(resolve => setImmediate(resolve));
    if (action === 'EOF') streams.input.end();
    else streams.signals.emit(action);
    await rejected;
    assert.equal(calls, 0);
    assert.equal(streams.signals.listenerCount('SIGINT'), 0);
    assert.equal(streams.signals.listenerCount('SIGTERM'), 0);
    assert.equal(streams.input.listenerCount('data'), 0);
  }
});

test('source release validation runs before discovery or prompt', async () => {
  for (const command of ['install', 'update', 'remove', 'status']) {
    await assert.rejects(manageHosts(command, {}, {
      select: () => assert.fail('Unconfigured release must fail before selection'),
      run: () => assert.fail('Unconfigured release must not call host'),
    }), {code: 'UNCONFIGURED_RELEASE'});
  }
});

test('single-host output remains unchanged for all maintenance commands', async () => {
  for (const command of ['install', 'update', 'remove', 'status']) {
    const expected = {command, host: 'claude', installed: command !== 'remove', version: '0.3.0'};
    const result = await manageHosts(command, {claude: '/native Claude'}, {
      validate: () => {},
      run: async (received, options) => {
        assert.equal(received, command);
        assert.deepEqual(options, {host: 'claude', claude: '/native Claude'});
        return expected;
      },
    });
    assert.strictEqual(result, expected);
  }
});

test('both hosts run sequentially with separate overrides and preserve partial success', async () => {
  for (const failedHost of [null, 'codex', 'claude']) {
    const calls = [];
    let active = false;
    const result = await manageHosts('install', {host: 'both', codex: '/a', claude: '/b'}, {
      validate: () => {},
      run: async (command, options) => {
        assert.equal(active, false);
        active = true;
        calls.push(options);
        await new Promise(resolve => setImmediate(resolve));
        active = false;
        if (options.host === failedHost) throw new InstallerError('native installation failed', 'NATIVE_COMMAND_FAILED');
        return {command, host: options.host, changed: true, installed: true};
      },
    });
    assert.deepEqual(calls, [{host: 'codex', codex: '/a'}, {host: 'claude', claude: '/b'}]);
    assert.equal(result.command, 'install');
    assert.equal(result.success, failedHost === null);
    assert.deepEqual(result.results.map(item => item.host), ['codex', 'claude'].filter(host => host !== failedHost));
    assert.deepEqual(result.errors, failedHost ? [{host: failedHost, error: 'native installation failed', code: 'NATIVE_COMMAND_FAILED'}] : []);
  }
});
