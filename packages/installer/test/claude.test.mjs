import assert from 'node:assert/strict';
import {test} from 'node:test';
import {InstallerError, RELEASE as SOURCE_RELEASE, describe, manage} from '../lib/installer.mjs';

const RELEASE = {...SOURCE_RELEASE, repository: 'delm-fixture/distribution', url: 'https://github.com/delm-fixture/distribution.git'};

function fake({installed = false, enabled = true, legacy = false} = {}) {
  const marketplace = {name: 'delm', source: 'git', url: RELEASE.url, ref: RELEASE.ref};
  const plugin = {id: RELEASE.plugin, version: '0.3.0', scope: 'user', enabled};
  const other = {id: 'other@elsewhere', version: '7.0.0', scope: 'user', enabled: true};
  const state = {marketplaces: installed ? [marketplace] : [], plugins: [other, ...(installed ? [plugin] : []),
    ...(legacy ? [{id: 'delm@delm-local', scope: 'user', version: '0.2.0', enabled: false}] : [])], calls: [], intercept: null};
  const run = async (file, args) => {
    state.calls.push([file, ...args]);
    const intercepted = await state.intercept?.(file, args);
    if (intercepted) return intercepted;
    if (file === 'git') return {stdout: 'git version fixture\n'};
    assert.equal(file, '/existing Claude');
    if (args.length === 1 && args[0] === '--version') return {stdout: '2.1.289 (Claude Code)\n'};
    assert.equal(args.at(-1), '--json');
    const operation = args.slice(1, -1);
    if (operation.join(' ') === 'marketplace list') return {stdout: JSON.stringify(state.marketplaces, null, 2)};
    if (operation.join(' ') === 'list') return {stdout: JSON.stringify(state.plugins, null, 2)};
    let result;
    if (operation[0] === 'marketplace') {
      if (operation[1] === 'add') {
        assert.deepEqual(operation, ['marketplace', 'add', `${RELEASE.url}#marketplace`, '--scope', 'user']);
        state.marketplaces.push(marketplace);
      } else assert.deepEqual(operation, ['marketplace', 'update', 'delm']);
      result = {command: `marketplace-${operation[1]}`, outcome: 'ok', marketplace: 'delm'};
    } else {
      assert.deepEqual(operation.slice(1), [RELEASE.plugin, '--scope', 'user', ...(operation[0] === 'uninstall' ? ['--keep-data'] : [])]);
      const current = state.plugins.find(item => item.id === RELEASE.plugin);
      if (operation[0] === 'install') state.plugins.push({...plugin, enabled: true});
      else if (operation[0] === 'enable') current.enabled = true;
      else if (operation[0] === 'update') current.version = '0.4.0';
      else if (operation[0] === 'uninstall') state.plugins = state.plugins.filter(item => item.id !== RELEASE.plugin);
      else assert.fail(`Unexpected mutation ${operation}`);
      result = {command: operation[0], outcome: 'ok', pluginId: RELEASE.plugin, scope: 'user'};
    }
    return {stdout: `Native progress\n${JSON.stringify(result)}\n`};
  };
  return {state, options: {host: 'claude', claude: '/existing Claude', platform: 'darwin', release: RELEASE, run, checkMaintenance: async () => {}}};
}

const mutations = state => state.calls.filter(call => call[1] === 'plugin' && call[2] !== 'list' && !(call[2] === 'marketplace' && call[3] === 'list'));

test('Claude mutation preflight preserves active work and status remains readable', async () => {
  for (const command of ['install', 'update', 'remove']) {
    const {state, options} = fake({installed: true, enabled: false});
    options.checkMaintenance = async ({host}) => {
      assert.equal(host, 'claude');
      throw new InstallerError('Stop active work first', 'ACTIVE_DELM_RUN');
    };
    await assert.rejects(manage(command, options), {code: 'ACTIVE_DELM_RUN'});
    assert.deepEqual(mutations(state), []);
    const result = await manage('status', options);
    assert.equal(result.readiness.installation, 'disabled');
    assert.match(describe(result), /Session readiness has not been checked/);
  }
});

test('Claude installation uses explicit user scope, fixed source/ref, and no Codex commands', async () => {
  const {state, options} = fake();
  const other = structuredClone(state.plugins[0]);
  const result = await manage('install', options);
  assert.equal(result.host, 'claude');
  assert.equal(result.scope, 'user');
  assert.equal(result.enabled, true);
  assert.deepEqual(state.plugins[0], other);
  assert.deepEqual(mutations(state).map(item => item.slice(2, -1)), [
    ['marketplace', 'add', `${RELEASE.url}#marketplace`, '--scope', 'user'], ['install', RELEASE.plugin, '--scope', 'user'],
  ]);
  assert.match(describe(result), /\/delm:run/);
  assert.doesNotMatch(describe(result), /Codex|\/hooks|\$delm/);
});

test('Claude repeated install is read-only; disabled install enables without reinstall', async () => {
  for (const enabled of [true, false]) {
    const {state, options} = fake({installed: true, enabled});
    const result = await manage('install', options);
    assert.equal(result.enabled, true);
    assert.equal(result.changed, !enabled);
    assert.deepEqual(mutations(state).map(call => call[2]), enabled ? [] : ['enable']);
  }
});

test('Claude update refreshes only its catalog and plugin and preserves disabled state', async () => {
  const {state, options} = fake({installed: true, enabled: false});
  const result = await manage('update', options);
  assert.equal(result.version, '0.4.0');
  assert.equal(result.enabled, false);
  assert.deepEqual(mutations(state).map(call => call.slice(2, -1)), [
    ['marketplace', 'update', 'delm'], ['update', RELEASE.plugin, '--scope', 'user'],
  ]);
  assert.match(describe(result), /remains disabled/);
});

test('Claude removal retains native data, marketplaces, legacy and unrelated plugins', async () => {
  const {state, options} = fake({installed: true, legacy: true});
  const markets = structuredClone(state.marketplaces);
  assert.equal((await manage('remove', options)).installed, false);
  assert.equal((await manage('remove', options)).changed, false);
  assert.deepEqual(state.marketplaces, markets);
  assert.deepEqual(state.plugins.map(item => item.id), ['other@elsewhere', 'delm@delm-local']);
  assert.deepEqual(mutations(state).map(call => call.slice(2, -1)), [['uninstall', RELEASE.plugin, '--scope', 'user', '--keep-data']]);
});

test('Claude wrong or missing source/ref, duplicate identities and other scopes fail before mutation', async () => {
  const changes = [
    state => { state.marketplaces[0].url = 'https://github.com/other/repo.git'; },
    state => { state.marketplaces[0].ref = 'main'; },
    state => { delete state.marketplaces[0].ref; },
    state => { state.marketplaces[0].source = 'directory'; },
    state => { state.marketplaces.push({...state.marketplaces[0]}); },
    state => { state.marketplaces = []; },
    state => { state.plugins.at(-1).scope = 'project'; },
    state => { state.plugins.push({...state.plugins.at(-1)}); },
  ];
  for (const change of changes) {
    for (const command of ['install', 'update', 'remove']) {
      const {state, options} = fake({installed: true});
      change(state);
      await assert.rejects(manage(command, options), error => ['MARKETPLACE_CONFLICT', 'SCOPE_CONFLICT'].includes(error.code));
      assert.deepEqual(mutations(state), []);
    }
  }
});

test('Claude legacy, missing installation, host mismatch and unsupported platform fail safely', async () => {
  const {state, options} = fake({legacy: true});
  for (const command of ['install', 'update']) await assert.rejects(manage(command, options), {code: 'LEGACY_INSTALLATION'});
  assert.deepEqual(mutations(state), []);
  const empty = fake();
  await assert.rejects(manage('update', empty.options), error => error.code === 'NOT_INSTALLED' && error.message.includes('--host claude'));
  const noCalls = {run: () => assert.fail('Called native tool for invalid invocation'), release: RELEASE};
  for (const options of [{host: 'wrong'}, {host: 'claude', codex: '/codex'}, {host: 'codex', claude: '/claude'}, {host: 'claude', claude: ''}]) {
    await assert.rejects(manage('status', {...noCalls, ...options}), {code: 'USAGE'});
  }
  await assert.rejects(manage('install', {...options, platform: 'win32'}), {code: 'UNSUPPORTED_PLATFORM'});
});

test('Claude status is read-only, supports other platforms and reports scope/source problems', async () => {
  const {state, options} = fake({installed: true});
  state.marketplaces[0].ref = 'wrong';
  state.plugins.at(-1).scope = 'local';
  const result = await manage('status', {...options, platform: 'linux'});
  assert.equal(result.scopeConflict, true);
  assert.equal(result.conflict, true);
  assert.equal(result.installed, false);
  assert.match(describe(result), /another scope/);
  assert.deepEqual(mutations(state), []);
});

test('Claude accepts documented GitHub source shape only with matching repository and ref', async () => {
  const {state, options} = fake({installed: true});
  state.marketplaces = [{name: 'delm', source: 'github', repo: RELEASE.repository, ref: 'marketplace'}];
  assert.equal((await manage('install', options)).changed, false);
  state.marketplaces[0].repo = 'other/repo';
  await assert.rejects(manage('install', options), {code: 'MARKETPLACE_CONFLICT'});
});

test('Claude malformed JSON and list schema are rejected before mutation', async () => {
  for (const stdout of ['not-json', '{}', '[null]']) {
    const {options} = fake();
    await assert.rejects(manage('install', {...options, run: async (_file, args) => ({stdout: args[0] === '--version' ? '2.1.289 (Claude Code)\n' : stdout})}), {code: 'UNSUPPORTED_CLAUDE'});
  }
});

test('Claude validates mutation command, identity and scope and preserves failed partial state', async () => {
  for (const field of ['command', 'pluginId', 'scope', 'outcome']) {
    const {state, options} = fake();
    state.intercept = (_file, args) => {
      if (args[1] === 'install') return {stdout: JSON.stringify({command: 'install', pluginId: RELEASE.plugin, scope: 'user', outcome: 'ok', [field]: 'wrong'})};
    };
    await assert.rejects(manage('install', options), error => ['UNEXPECTED_IDENTITY', 'UNSUPPORTED_CLAUDE'].includes(error.code));
    assert.equal(state.marketplaces[0].name, 'delm');
    assert.ok(!mutations(state).some(call => ['uninstall', 'remove'].includes(call[2])));
  }
});

test('Claude accepts only the exact already-enabled native failure after verifying final state', async () => {
  const {state, options} = fake({installed: true, enabled: false});
  state.intercept = (_file, args) => {
    if (args[1] !== 'enable') return;
    state.plugins.at(-1).enabled = true;
    const error = new InstallerError('already enabled', 'NATIVE_COMMAND_FAILED');
    error.stdout = JSON.stringify({command: 'enable', plugin: RELEASE.plugin, scope: 'user', outcome: 'failed', failureCode: 'already_in_goal_state', alreadyInGoalState: true});
    throw error;
  };
  assert.equal((await manage('install', options)).enabled, true);
  state.plugins.at(-1).enabled = false;
  state.intercept = () => { throw new InstallerError('native policy refuses plugin', 'NATIVE_COMMAND_FAILED'); };
  await assert.rejects(manage('install', options), /native policy refuses/);
});

test('Claude mutation failures preserve native diagnostics even when optional identity fields are absent', async () => {
  const {state, options} = fake();
  state.intercept = (_file, args) => args[1] === 'install' ? {stdout: JSON.stringify({command: 'install', outcome: 'failed', message: 'Organization policy blocks this plugin'})} : undefined;
  await assert.rejects(manage('install', options), error => error.code === 'NATIVE_COMMAND_FAILED' && /Organization policy/.test(error.message));
  assert.equal(state.marketplaces.length, 1);
  assert.equal(mutations(state).some(call => call.includes('--yes') || call.includes('--accept-command')), false);
});

test('Claude catalog identity change blocks install/update and false native success fails verification', async () => {
  for (const command of ['install', 'update']) {
    const {state, options} = fake({installed: command === 'update'});
    state.intercept = (_file, args) => {
      if (args[1] === 'marketplace' && ['add', 'update'].includes(args[2])) {
        state.marketplaces = [{name: 'delm', source: 'git', url: RELEASE.url, ref: 'unexpected'}];
        return {stdout: JSON.stringify({command: `marketplace-${args[2]}`, outcome: 'ok', marketplace: 'delm'})};
      }
    };
    await assert.rejects(manage(command, options), {code: 'VERIFICATION_FAILED'});
    assert.ok(!mutations(state).some(call => ['install', 'update'].includes(call[2])));
  }
  const {state, options} = fake();
  state.intercept = (_file, args) => args[1] === 'install' ? {stdout: JSON.stringify({command: 'install', outcome: 'ok', pluginId: RELEASE.plugin, scope: 'user'})} : undefined;
  await assert.rejects(manage('install', options), {code: 'VERIFICATION_FAILED'});
});


test('Claude install and update reject unsupported versions or a different executable before reading or changing state', async () => {
  for (const version of ['2.1.288 (Claude Code)', '2.0.999 (Claude Code)', '1.99.999 (Claude Code)',
    'codex-cli 0.160.0', '2.1.289', '2.1.289 (Claude Code)\nextra', '2.1.289-beta (Claude Code)',
    '9007199254740992.1.289 (Claude Code)']) {
    for (const command of ['install', 'update']) {
      const {state, options} = fake({installed: command === 'update'});
      state.intercept = (_file, args) => args[0] === '--version' ? {stdout: version} : undefined;
      await assert.rejects(manage(command, options), error =>
        ['UNSUPPORTED_CLAUDE_VERSION', 'INVALID_CLAUDE_EXECUTABLE'].includes(error.code));
      assert.deepEqual(state.calls, [['/existing Claude', '--version']]);
      assert.equal(state.plugins.some(item => item.id === RELEASE.plugin), command === 'update');
    }
  }
});

test('Claude qualified floor and later stable versions pass; old-version status and removal remain available', async () => {
  for (const version of ['2.1.289', '2.1.290', '2.2.0', '3.0.0']) {
    const {state, options} = fake();
    state.intercept = (_file, args) => args[0] === '--version' ? {stdout: version + ' (Claude Code)\n'} : undefined;
    assert.equal((await manage('install', options)).installed, true);
    assert.deepEqual(state.calls[0], ['/existing Claude', '--version']);
  }
  for (const command of ['status', 'remove']) {
    const {state, options} = fake({installed: true});
    state.intercept = (_file, args) => { assert.notEqual(args[0], '--version'); };
    assert.equal((await manage(command, options)).installed, command === 'status');
  }
});
