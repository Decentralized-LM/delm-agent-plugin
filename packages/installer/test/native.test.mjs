import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {test} from 'node:test';
import {RELEASE} from '../lib/installer.mjs';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

test('packed npx entrypoint installs, updates and removes through real Codex with isolated Git transport', {
  skip: process.platform !== 'darwin' ? 'Native installation currently supports macOS only.' : false,
  timeout: 120_000,
}, () => {
  const root = mkdtempSync(path.join(tmpdir(), 'delm packed native '));
  try {
    const home = path.join(root, 'home');
    const codexHome = path.join(root, 'codex home');
    const repository = path.join(root, 'release repository');
    const output = path.join(root, 'packed');
    const gitconfig = path.join(root, 'gitconfig');
    for (const directory of [home, codexHome, repository, output]) mkdirSync(directory, {recursive: true});
    for (const name of ['user.npmrc', 'global.npmrc', 'gitconfig']) writeFileSync(path.join(root, name), '');
    const env = {
      ...process.env, HOME: home, CODEX_HOME: codexHome,
      GIT_CONFIG_GLOBAL: gitconfig, GIT_CONFIG_NOSYSTEM: '1', GIT_TERMINAL_PROMPT: '0',
      npm_config_userconfig: path.join(root, 'user.npmrc'),
      npm_config_globalconfig: path.join(root, 'global.npmrc'),
      npm_config_cache: path.join(root, 'npm cache'),
      npm_config_ignore_scripts: 'true', npm_config_update_notifier: 'false',
    };
    const codex = process.env.DELM_TEST_HOST || execFileSync('/usr/bin/which', ['codex'], {encoding: 'utf8'}).trim();
    const run = (file, args, cwd = root) => execFileSync(file, args, {cwd, env, encoding: 'utf8', timeout: 45_000, stdio: ['ignore', 'pipe', 'pipe']});
    const git = (...args) => run('git', args, repository);
    const native = (...args) => JSON.parse(run(codex, ['plugin', ...args, '--json']));
    run('git', ['config', '--file', gitconfig, `url.${pathToFileURL(repository).href}.insteadOf`, RELEASE.url]);
    git('init', '-b', 'marketplace');
    git('config', 'user.name', 'DeLM installer fixture');
    git('config', 'user.email', 'fixture@example.invalid');
    writeFileSync(path.join(codexHome, 'config.toml'), 'model = "fixture-model"\n');
    writeFileSync(path.join(codexHome, 'auth.json'), '{}\n');
    const retained = path.join(codexHome, 'delm/runs/result.txt');
    mkdirSync(path.dirname(retained), {recursive: true});
    writeFileSync(retained, 'retained work');

    function writeJson(relative, value) {
      const destination = path.join(repository, relative);
      mkdirSync(path.dirname(destination), {recursive: true});
      writeFileSync(destination, JSON.stringify(value, null, 2));
    }
    function release(version) {
      writeJson('plugins/delm/.codex-plugin/plugin.json', {name: 'delm', version, skills: './skills', hooks: './hooks/hooks.json'});
      writeJson('plugins/delm/hooks/hooks.json', {hooks: {Stop: [{hooks: [{type: 'command', command: 'exec "${PLUGIN_ROOT}/bin/delm" lifecycle-hook', timeout: 5}]}]}});
      const skill = path.join(repository, 'plugins/delm/skills/run/SKILL.md');
      mkdirSync(path.dirname(skill), {recursive: true});
      writeFileSync(skill, '---\nname: run\ndescription: DeLM installer fixture.\n---\nDo not run a model.\n');
      const binary = path.join(repository, 'plugins/delm/bin/delm');
      mkdirSync(path.dirname(binary), {recursive: true});
      writeFileSync(binary, `#!/bin/sh\nprintf 'delm ${version}\\n'\n`);
      chmodSync(binary, 0o755);
      writeJson('.agents/plugins/marketplace.json', {name: 'delm', plugins: [{name: 'delm', source: {
        source: 'git-subdir', url: RELEASE.url, path: './plugins/delm', ref: `delm-plugin-v${version}`,
      }}]});
      git('add', '.');
      git('commit', '-m', `Fixture ${version}`);
      git('tag', `delm-plugin-v${version}`);
    }
    release('0.3.0');
    const unrelated = path.join(root, 'unrelated marketplace');
    mkdirSync(path.join(unrelated, '.agents/plugins'), {recursive: true});
    mkdirSync(path.join(unrelated, 'plugin/.codex-plugin'), {recursive: true});
    writeFileSync(path.join(unrelated, '.agents/plugins/marketplace.json'), JSON.stringify({name: 'unrelated', plugins: [{name: 'other', source: {source: 'local', path: './plugin'}}]}));
    writeFileSync(path.join(unrelated, 'plugin/.codex-plugin/plugin.json'), JSON.stringify({name: 'other', version: '7.0.0'}));
    native('marketplace', 'add', unrelated);
    native('add', 'other@unrelated');
    const otherBefore = native('list', '--marketplace', 'unrelated');
    const packed = JSON.parse(run('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', output], packageRoot));
    const tarball = path.join(output, packed[0].filename);
    const cli = (...args) => JSON.parse(run('npx', ['--yes', '--offline', `--package=${tarball}`, 'delm-agent', ...args, '--codex', codex, '--json']));

    assert.equal(cli('status').installed, false);
    assert.equal(cli('install').version, '0.3.0');
    assert.equal(cli('install').changed, false);
    const originalConfig = readFileSync(path.join(codexHome, 'config.toml'), 'utf8');
    assert.ok(!originalConfig.includes('trusted_hash'), 'Installation must not grant hook trust.');
    assert.equal(readFileSync(path.join(codexHome, 'auth.json'), 'utf8'), '{}\n');

    release('0.3.1');
    assert.equal(cli('update').version, '0.3.1');
    const installed = native('list', '--marketplace', 'delm').installed;
    assert.equal(installed.find(item => item.pluginId === RELEASE.plugin).version, '0.3.1');
    assert.equal(run(path.join(codexHome, 'plugins/cache/delm/delm/0.3.1/bin/delm'), []).trim(), 'delm 0.3.1');
    assert.equal(cli('remove').installed, false);
    assert.equal(cli('remove').changed, false);
    assert.ok(native('marketplace', 'list').marketplaces.some(item => item.name === 'delm'));
    assert.deepEqual(native('list', '--marketplace', 'unrelated'), otherBefore);
    assert.equal(readFileSync(retained, 'utf8'), 'retained work');
    assert.equal(readFileSync(path.join(codexHome, 'auth.json'), 'utf8'), '{}\n');
    assert.match(readFileSync(path.join(codexHome, 'config.toml'), 'utf8'), /model = "fixture-model"/);

    // The native list API omits the registered ref. The installer delegates this
    // check to native add; verify that a different ref cannot be silently reset.
    git('branch', 'other-ref');
    native('marketplace', 'remove', 'delm');
    native('marketplace', 'add', RELEASE.url, '--ref', 'other-ref');
    native('add', RELEASE.plugin);
    const conflictingConfig = readFileSync(path.join(codexHome, 'config.toml'), 'utf8');
    const conflictingPlugin = native('list', '--marketplace', 'delm');
    const conflictingRuntime = readFileSync(path.join(codexHome, 'plugins/cache/delm/delm/0.3.1/bin/delm'));
    assert.throws(() => cli('install'), error => error.status === 1
      && String(error.stderr).includes('NATIVE_COMMAND_FAILED'));
    assert.equal(readFileSync(path.join(codexHome, 'config.toml'), 'utf8'), conflictingConfig);
    assert.deepEqual(native('list', '--marketplace', 'delm'), conflictingPlugin);
    assert.deepEqual(readFileSync(path.join(codexHome, 'plugins/cache/delm/delm/0.3.1/bin/delm')), conflictingRuntime);
    assert.deepEqual(native('list', '--marketplace', 'unrelated'), otherBefore);
  } finally {
    rmSync(root, {recursive: true, force: true});
  }
});
