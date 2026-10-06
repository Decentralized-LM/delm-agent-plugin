import assert from 'node:assert/strict';
import {chmod, mkdtemp, readFile, stat, writeFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {test} from 'node:test';
import {COMMANDS, InstallerError, RELEASE as SOURCE_RELEASE, describe, execute, manage} from '../lib/installer.mjs';

const RELEASE = {...SOURCE_RELEASE, repository: 'delm-fixture/distribution', url: 'https://github.com/delm-fixture/distribution.git'};

function fake({installed = false, enabled = true, source = RELEASE.url, legacy = false} = {}) {
  const unrelated = {pluginId: 'other@elsewhere', installed: true, enabled: true, version: '7.0.0'};
  const state = {
    marketplaces: installed ? [{name: 'delm', marketplaceSource: {sourceType: 'git', source}}] : [],
    plugins: [unrelated, ...(installed ? [{pluginId: RELEASE.plugin, installed: true, enabled, version: '0.3.0'}] : [])],
    legacy, calls: [], fail: null,
  };
  const run = async (file, args) => {
    state.calls.push([file, ...args]);
    if (state.fail) await state.fail(file, args);
    if (file === 'git') return {stdout: 'git version fixture\n'};
    assert.equal(file, '/existing Codex');
    assert.equal(args.at(-1), '--json');
    const operation = args.slice(1, -1);
    let result;
    if (operation.join(' ') === 'marketplace list') result = {marketplaces: state.marketplaces};
    else if (operation[0] === 'list') result = {installed: operation[2] === 'delm-local'
      ? state.legacy ? [{pluginId: 'delm@delm-local', installed: true}] : []
      : state.plugins.filter(item => item.pluginId === RELEASE.plugin)};
    else if (operation[0] === 'marketplace' && operation[1] === 'add') {
      const registeredSource = state.marketplaces.find(item => item.name === 'delm')?.marketplaceSource.source;
      assert.deepEqual(operation, ['marketplace', 'add', registeredSource ?? RELEASE.url, '--ref', RELEASE.ref]);
      if (!state.marketplaces.some(item => item.name === 'delm')) state.marketplaces.push({name: 'delm', marketplaceSource: {sourceType: 'git', source: RELEASE.url}});
      result = {marketplaceName: 'delm', alreadyAdded: installed};
    } else if (operation[0] === 'add') {
      assert.equal(operation[1], RELEASE.plugin);
      state.plugins = state.plugins.filter(item => item.pluginId !== RELEASE.plugin);
      state.plugins.push({pluginId: RELEASE.plugin, installed: true, enabled: true, version: '0.3.0'});
      result = {pluginId: RELEASE.plugin};
    } else if (operation[0] === 'marketplace' && operation[1] === 'upgrade') {
      assert.deepEqual(operation, ['marketplace', 'upgrade', 'delm']);
      state.plugins.find(item => item.pluginId === RELEASE.plugin).version = '0.4.0';
      result = {errors: [], upgradedRoots: ['/fixture']};
    } else if (operation[0] === 'remove') {
      assert.equal(operation[1], RELEASE.plugin);
      state.plugins = state.plugins.filter(item => item.pluginId !== RELEASE.plugin);
      result = {pluginId: RELEASE.plugin};
    } else assert.fail(`Unexpected native command ${operation}`);
    return {stdout: JSON.stringify(result)};
  };
  return {state, options: {codex: '/existing Codex', platform: 'darwin', run, release: RELEASE, checkMaintenance: async () => {}}};
}

const mutations = state => state.calls.filter(call => call[1] === 'plugin' && (
  ['add', 'remove'].includes(call[2]) || ['add', 'upgrade', 'remove'].includes(call[3])));

test('maintenance refusal precedes every Codex mutation and leaves status available', async () => {
  for (const command of ['install', 'update', 'remove']) {
    const {state, options} = fake({installed: true});
    options.checkMaintenance = async ({host}) => {
      assert.equal(host, 'codex');
      throw new InstallerError('Stop active work first', 'ACTIVE_DELM_RUN');
    };
    await assert.rejects(manage(command, options), {code: 'ACTIVE_DELM_RUN'});
    assert.deepEqual(mutations(state), []);
    const result = await manage('status', options);
    assert.equal(result.readiness.session, 'not_checked');
    assert.match(describe(result), /Session readiness has not been checked/);
  }
});

test('source and invalid release configurations fail before calling native tools', async () => {
  for (const command of COMMANDS) {
    const run = async () => assert.fail('Unconfigured release called a native tool');
    await assert.rejects(manage(command, {platform: 'darwin', run}), {code: 'UNCONFIGURED_RELEASE'});
    await assert.rejects(manage(command, {platform: 'darwin', run, release: {...RELEASE, repository: '../invalid'}}), {code: 'INVALID_RELEASE'});
  }
});

test('invalid repository transfer allowlists are rejected before either host is called', async () => {
  const invalid = [null, 'old/repo', {}, [null], [42], ['../repo'], ['old/repo/extra'], ['old/repo\n'],
    ['https://github.com/old/repo'], ['old/repo#marketplace'], ['old/repo', 'OLD/REPO'],
    [RELEASE.repository], [RELEASE.repository.toUpperCase()]];
  for (const host of ['codex', 'claude']) {
    for (const previousRepositories of invalid) {
      await assert.rejects(manage('install', {
        host, platform: 'darwin', release: {...RELEASE, previousRepositories},
        run: async () => assert.fail('Invalid release called a native tool'),
      }), {code: 'INVALID_RELEASE'});
    }
  }
});

test('Codex preserves approved previous registration and still asks native add to validate its ref', async () => {
  for (const command of COMMANDS) {
    for (const suffix of ['', '.git']) {
      const source = `https://github.com/old-owner/old-repo${suffix}`;
      const {state, options} = fake({installed: true, source});
      options.release = {...RELEASE, previousRepositories: ['old-owner/old-repo']};
      const before = structuredClone(state.marketplaces);
      const result = await manage(command, options);
      assert.equal(result.conflict, false);
      assert.deepEqual(state.marketplaces, before);
      if (['install', 'update'].includes(command)) {
        assert.deepEqual(mutations(state)[0].slice(2, -1), ['marketplace', 'add', source, '--ref', 'marketplace']);
      }
    }
  }
  const {state, options} = fake();
  options.release = {...RELEASE, previousRepositories: ['old-owner/old-repo']};
  await manage('install', options);
  assert.equal(state.marketplaces[0].marketplaceSource.source, RELEASE.url);
});

test('Codex rejects unapproved and altered previous source URLs without mutations', async () => {
  for (const source of ['https://github.com/unrelated/repo.git', 'https://github.com/old-owner/old-repo.git?other',
    'https://github.com/old-owner/old-repo/extra', 'https://github.com/old-owner/old-repo.git#marketplace',
    'https://user@github.com/old-owner/old-repo.git', 'http://github.com/old-owner/old-repo.git']) {
    const {state, options} = fake({installed: true, source});
    options.release = {...RELEASE, previousRepositories: ['old-owner/old-repo']};
    await assert.rejects(manage('install', options), {code: 'MARKETPLACE_CONFLICT'});
    assert.deepEqual(mutations(state), []);
  }
});

test('installation uses only the fixed native marketplace and preserves unrelated plugins', async () => {
  const {state, options} = fake();
  const other = structuredClone(state.plugins[0]);
  const result = await manage('install', options);
  assert.equal(result.version, '0.3.0');
  assert.equal(result.enabled, true);
  assert.equal(result.changed, true);
  assert.deepEqual(state.plugins[0], other);
  assert.deepEqual(mutations(state).map(call => call.slice(2, -1)), [
    ['marketplace', 'add', RELEASE.url, '--ref', 'marketplace'], ['add', 'delm@delm'],
  ]);
});

test('repeated install does not reinstall an enabled plugin; disabled install explicitly enables it', async () => {
  for (const enabled of [true, false]) {
    const {state, options} = fake({installed: true, enabled});
    const result = await manage('install', options);
    assert.equal(result.enabled, true);
    assert.equal(result.changed, !enabled);
    assert.equal(mutations(state).filter(call => call[2] === 'add').length, enabled ? 0 : 1);
    const message = describe(result);
    if (enabled) {
      assert.match(message, /already installed and enabled\. No reinstall was needed/);
      assert.match(message, /If DeLM is unavailable.*review its hooks/);
      assert.doesNotMatch(message, /^Restart/m);
    } else {
      assert.match(message, /Installed DeLM/);
      assert.match(message, /Restart Codex, open \/hooks/);
    }
  }
});

test('update uses native upgrade and verifies its result without a second plugin add', async () => {
  const {state, options} = fake({installed: true, enabled: false});
  const result = await manage('update', options);
  assert.equal(result.version, '0.4.0');
  assert.equal(result.enabled, false);
  assert.equal(mutations(state).filter(call => call[2] === 'add').length, 0);
  assert.equal(mutations(state).filter(call => call[3] === 'upgrade').length, 1);
  assert.match(describe(result), /remains disabled.*install command to enable/);
  assert.doesNotMatch(describe(result), /use \$delm:run/);
});

test('status distinguishes source and public installations without implying source activation', async () => {
  for (const installed of [false, true]) {
    const {state, options} = fake({installed, legacy: true});
    const result = await manage('status', options);
    const message = describe(result);
    assert.match(message, /delm@delm-local/);
    assert.doesNotMatch(message, /DeLM is not installed/);
    if (installed) assert.match(message, /DeLM 0\.3\.0 \(delm@delm\) is enabled/);
    else assert.match(message, /public plugin \(delm@delm\) is not installed/);
    assert.deepEqual(mutations(state), []);
  }
});

test('conflicting, unknown, and legacy registrations are rejected before any mutation', async () => {
  for (const fixture of [
    {installed: true, source: 'https://github.com/other/project.git'},
    {installed: true, source: undefined, legacy: true},
    {legacy: true},
  ]) {
    const {state, options} = fake(fixture);
    await assert.rejects(manage('install', options), error => ['MARKETPLACE_CONFLICT', 'LEGACY_INSTALLATION'].includes(error.code));
    assert.deepEqual(mutations(state), []);
  }
  const {state, options} = fake({installed: true});
  delete state.marketplaces[0].marketplaceSource;
  await assert.rejects(manage('remove', options), {code: 'MARKETPLACE_CONFLICT'});
  assert.deepEqual(mutations(state), []);
});

test('remove retains the marketplace and unrelated registrations; missing install/update stays harmless', async () => {
  const {state, options} = fake({installed: true});
  const marketplaces = structuredClone(state.marketplaces);
  assert.equal((await manage('remove', options)).changed, true);
  assert.equal((await manage('remove', options)).changed, false);
  assert.deepEqual(state.marketplaces, marketplaces);
  assert.deepEqual(state.plugins.map(item => item.pluginId), ['other@elsewhere']);
  await assert.rejects(manage('update', options), error => error.code === 'NOT_INSTALLED'
    && error.message.includes('npx --yes delm-agent@latest install'));
  assert.deepEqual(mutations(state).map(call => call.slice(2, -1)), [['remove', RELEASE.plugin]]);
});

test('unsupported OS is rejected before native mutations while status remains available', async () => {
  const {state, options} = fake();
  for (const command of COMMANDS.filter(item => item !== 'status')) {
    await assert.rejects(manage(command, {...options, platform: 'win32'}), {code: 'UNSUPPORTED_PLATFORM'});
  }
  assert.deepEqual(state.calls, []);
  assert.equal((await manage('status', {...options, platform: 'linux'})).installed, false);
});

test('native errors are relayed and partial installation is not rolled back destructively', async () => {
  const {state, options} = fake();
  state.fail = (_file, args) => {
    if (args[1] === 'add') throw new InstallerError('native download failed', 'NATIVE_COMMAND_FAILED');
  };
  await assert.rejects(manage('install', options), /native download failed/);
  assert.equal(state.marketplaces[0].name, 'delm');
  assert.equal(mutations(state).some(call => call.includes('remove')), false);
});

test('unsupported native JSON is diagnosed before mutation', async () => {
  for (const stdout of ['not-json', '{}']) {
    await assert.rejects(manage('install', {platform: 'darwin', release: RELEASE, run: async () => ({stdout})}), {code: 'UNSUPPORTED_CODEX'});
  }
});

test('execution handles paths with spaces without a shell and preserves native stderr', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'delm fake CLI '));
  try {
    const executable = path.join(root, 'fake codex.mjs');
    await writeFile(executable, `process.stderr.write('native failure details'); process.exit(9);\n`);
    await assert.rejects(execute(process.execPath, [executable]), error => error.code === 'NATIVE_COMMAND_FAILED' && error.message.includes('native failure details'));
    await assert.rejects(execute(path.join(root, 'missing codex'), []), {code: 'MISSING_EXECUTABLE'});
  } finally {
    await rm(root, {recursive: true, force: true});
  }
});

test('an empty legacy marketplace is harmless and retained', async () => {
  const {state, options} = fake();
  const legacy = {name: 'delm-local', marketplaceSource: {sourceType: 'local', source: '/old checkout'}};
  state.marketplaces.push(legacy);
  assert.equal((await manage('install', options)).installed, true);
  assert.ok(state.marketplaces.includes(legacy));
});


test('relative Codex paths use one private working directory which is cleaned up', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'delm native cwd '));
  try {
    const codex = path.join(root, 'fake codex.mjs');
    const log = path.join(root, 'calls.jsonl');
    await writeFile(codex, `#!/usr/bin/env node\nimport {appendFileSync} from 'node:fs';\nappendFileSync(${JSON.stringify(log)}, JSON.stringify({cwd:process.cwd(), args:process.argv.slice(2)})+'\\n');\nconsole.log(JSON.stringify(process.argv[3] === 'marketplace' ? {marketplaces:[]} : {installed:[]}));\n`);
    await chmod(codex, 0o755);
    const result = await manage('status', {codex: path.relative(process.cwd(), codex), release: RELEASE});
    assert.equal(result.installed, false);
    const calls = (await readFile(log, 'utf8')).trim().split('\n').map(line => JSON.parse(line));
    assert.equal(calls.length, 3);
    assert.equal(new Set(calls.map(call => call.cwd)).size, 1);
    assert.notEqual(calls[0].cwd, process.cwd());
    assert.notEqual(calls[0].cwd, tmpdir());
    await assert.rejects(stat(calls[0].cwd), {code: 'ENOENT'});
  } finally {
    await rm(root, {recursive: true, force: true});
  }
});


test('explicit removal of the verified public plugin preserves an installed legacy plugin', async () => {
  const {state, options} = fake({installed: true, legacy: true});
  const result = await manage('remove', options);
  assert.equal(result.installed, false);
  assert.equal(result.legacyInstalled, true);
  assert.equal(state.legacy, true);
  assert.match(describe(result), /Removed DeLM \(delm@delm\)/);
  assert.match(describe(result), /source installation \(delm@delm-local\) remains installed/);
  const repeated = await manage('remove', options);
  assert.match(describe(repeated), /public plugin \(delm@delm\) is not installed\. Nothing changed/);
  assert.match(describe(repeated), /source installation \(delm@delm-local\) remains installed/);
  assert.deepEqual(mutations(state).map(call => call.slice(2, -1)), [['remove', RELEASE.plugin]]);
});
