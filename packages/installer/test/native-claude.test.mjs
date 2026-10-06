import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {test} from 'node:test';

const packageRoot = fileURLToPath(new URL('..', import.meta.url));

test('packed installer transfers real Claude registration with isolated local transport and retained data', {
  skip: process.platform !== 'darwin' ? 'Native installation currently supports macOS only.' : false,
  timeout: 120_000,
}, t => {
  const root = mkdtempSync(path.join(tmpdir(), 'delm native Claude '));
  try {
    const home = path.join(root, 'home');
    const config = path.join(root, 'claude config');
    const codexHome = path.join(root, 'codex untouched');
    const repository = path.join(root, 'release repository');
    const output = path.join(root, 'prepared');
    const gitconfig = path.join(root, 'gitconfig');
    for (const directory of [home, config, codexHome, repository]) mkdirSync(directory, {recursive: true});
    for (const name of ['user.npmrc', 'global.npmrc', 'gitconfig']) writeFileSync(path.join(root, name), '');
    const env = {
      ...process.env, HOME: home, CLAUDE_CONFIG_DIR: config, CODEX_HOME: codexHome,
      GIT_CONFIG_GLOBAL: gitconfig, GIT_CONFIG_NOSYSTEM: '1', GIT_TERMINAL_PROMPT: '0',
      npm_config_userconfig: path.join(root, 'user.npmrc'),
      npm_config_globalconfig: path.join(root, 'global.npmrc'),
      npm_config_cache: path.join(root, 'npm cache'),
      npm_config_ignore_scripts: 'true', npm_config_update_notifier: 'false',
    };
    for (const key of Object.keys(env)) {
      if (/^(ANTHROPIC_|CLAUDE_CODE_OAUTH_TOKEN|CLAUDE_CODE_SESSION_ACCESS_TOKEN)/.test(key)) delete env[key];
    }
    const claude = process.env.DELM_TEST_CLAUDE || execFileSync('/usr/bin/which', ['claude'], {encoding: 'utf8'}).trim();
    const run = (file, args, cwd = root) => execFileSync(file, args, {cwd, env, encoding: 'utf8', timeout: 45_000, stdio: ['ignore', 'pipe', 'pipe']});
    const hostVersion = run(claude, ['--version']).trim();
    t.diagnostic(`Native distribution fixture: ${hostVersion}; no model calls or account login.`);
    const prepared = JSON.parse(run('python3', [path.resolve(packageRoot, '../../scripts/prepare_installer.py'),
      '--repository', 'delm-fixture/claude-distribution', '--out', output]));
    const url = 'https://github.com/delm-fixture/claude-distribution.git';
    const native = (...args) => {
      const raw = run(claude, ['plugin', ...args, '--json']).trim();
      return JSON.parse(raw.startsWith('[') ? raw : raw.split(/\r?\n/).at(-1));
    };
    const git = (...args) => run('git', args, repository);
    run('git', ['config', '--file', gitconfig, `url.${pathToFileURL(repository).href}.insteadOf`, url]);
    git('init', '-b', 'marketplace');
    git('config', 'user.name', 'DeLM installer fixture');
    git('config', 'user.email', 'fixture@example.invalid');
    const originalSettings = {model: 'fixture-model', permissions: {deny: ['Bash(unwanted-command)']}};
    writeFileSync(path.join(config, 'settings.json'), JSON.stringify(originalSettings));
    writeFileSync(path.join(config, '.credentials.json'), '{}\n');
    writeFileSync(path.join(codexHome, 'config.toml'), 'model = "codex-untouched"\n');
    function writeJson(base, relative, value) {
      const destination = path.join(base, relative);
      mkdirSync(path.dirname(destination), {recursive: true});
      writeFileSync(destination, JSON.stringify(value, null, 2));
    }
    function release(version) {
      writeJson(repository, 'plugins/delm-claude/.claude-plugin/plugin.json', {name: 'delm', version, description: 'DeLM installer fixture'});
      const skill = path.join(repository, 'plugins/delm-claude/skills/run/SKILL.md');
      mkdirSync(path.dirname(skill), {recursive: true});
      writeFileSync(skill, '---\nname: run\ndescription: Installer fixture; no model calls.\n---\nDo not execute this fixture skill.\n');
      const binary = path.join(repository, 'plugins/delm-claude/bin/delm');
      mkdirSync(path.dirname(binary), {recursive: true});
      writeFileSync(binary, `#!/bin/sh\nprintf 'delm ${version}\\n'\n`);
      chmodSync(binary, 0o755);
      writeJson(repository, '.claude-plugin/marketplace.json', {name: 'delm', owner: {name: 'DeLM fixture'},
        description: 'Local fixture marketplace', plugins: [{name: 'delm', source: './plugins/delm-claude'}]});
      git('add', '.');
      git('commit', '-m', `Fixture ${version}`);
    }
    release('0.3.0');
    const unrelated = path.join(root, 'unrelated marketplace');
    writeJson(unrelated, '.claude-plugin/marketplace.json', {name: 'unrelated', owner: {name: 'Fixture'}, plugins: [{name: 'other', source: './plugin'}]});
    writeJson(unrelated, 'plugin/.claude-plugin/plugin.json', {name: 'other', version: '7.0.0'});
    native('marketplace', 'add', unrelated, '--scope', 'user');
    native('install', 'other@unrelated', '--scope', 'user');
    const otherBefore = native('list').find(item => item.id === 'other@unrelated');
    const tarball = path.join(output, prepared.tarball.path);
    const packedCli = packageTarball => (...args) => JSON.parse(run('npx', ['--yes', '--offline', `--package=${packageTarball}`, 'delm-agent', ...args, '--host', 'claude', '--claude', claude, '--json']));
    const cli = packedCli(tarball);
    assert.equal(cli('status').host, 'claude');
    assert.equal(cli('install').version, '0.3.0');
    const repeated = cli('install');
    assert.equal(repeated.changed, false);
    assert.equal(repeated.scope, 'user');

    const transferredRepository = 'delm-fixture/transferred-claude-distribution';
    const transferredUrl = `https://github.com/${transferredRepository}.git`;
    const transferredOutput = path.join(root, 'prepared after transfer');
    const transferred = JSON.parse(run('python3', [path.resolve(packageRoot, '../../scripts/prepare_installer.py'),
      '--repository', transferredRepository, '--previous-repository', 'delm-fixture/claude-distribution', '--out', transferredOutput]));
    // Both addresses reach the same fixture history, standing in for a GitHub
    // transfer redirect. No external repository is created or contacted.
    run('git', ['config', '--file', gitconfig, '--add', `url.${pathToFileURL(repository).href}.insteadOf`, transferredUrl]);
    const transferredCli = packedCli(path.join(transferredOutput, transferred.tarball.path));
    assert.equal(transferredCli('install').changed, false);
    assert.equal(native('marketplace', 'list').find(item => item.name === 'delm').url, url);
    const saved = path.join(config, 'plugins/data/delm-delm/retained.txt');
    mkdirSync(path.dirname(saved), {recursive: true});
    writeFileSync(saved, 'retained plugin data');
    native('disable', 'delm@delm', '--scope', 'user');
    release('0.3.1');
    const updated = transferredCli('update');
    assert.equal(updated.version, '0.3.1');
    assert.equal(updated.enabled, false);
    assert.equal(transferredCli('install').enabled, true);
    const installed = native('list').find(item => item.id === 'delm@delm');
    assert.equal(installed.scope, 'user');
    assert.equal(run(path.join(installed.installPath, 'bin/delm'), []).trim(), 'delm 0.3.1');
    assert.equal(transferredCli('remove').installed, false);
    assert.equal(transferredCli('remove').changed, false);
    assert.equal(readFileSync(saved, 'utf8'), 'retained plugin data');
    assert.ok(native('marketplace', 'list').some(item => item.name === 'delm'));
    assert.deepEqual(native('list').find(item => item.id === 'other@unrelated'), otherBefore);
    const settings = JSON.parse(readFileSync(path.join(config, 'settings.json'), 'utf8'));
    assert.equal(settings.model, originalSettings.model);
    assert.deepEqual(settings.permissions, originalSettings.permissions);
    assert.equal(readFileSync(path.join(config, '.credentials.json'), 'utf8'), '{}\n');
    assert.equal(readFileSync(path.join(codexHome, 'config.toml'), 'utf8'), 'model = "codex-untouched"\n');

    native('marketplace', 'remove', 'delm');
    assert.equal(transferredCli('install').version, '0.3.1');
    assert.equal(native('marketplace', 'list').find(item => item.name === 'delm').url, transferredUrl);
    assert.equal(transferredCli('remove').installed, false);

    // A correct repository at a different branch must not be silently replaced.
    git('branch', 'other-ref');
    native('marketplace', 'remove', 'delm');
    native('marketplace', 'add', `${url}#other-ref`, '--scope', 'user');
    native('install', 'delm@delm', '--scope', 'user');
    const registrationBefore = native('marketplace', 'list');
    const pluginBefore = native('list').find(item => item.id === 'delm@delm');
    assert.throws(() => transferredCli('install'), error => error.status === 1 && JSON.parse(error.stderr).code === 'MARKETPLACE_CONFLICT');
    assert.deepEqual(native('marketplace', 'list'), registrationBefore);
    assert.deepEqual(native('list').find(item => item.id === 'delm@delm'), pluginBefore);
    assert.equal(run(claude, ['--version']).trim(), hostVersion, 'Qualification must use one host version throughout.');
  } finally {
    rmSync(root, {recursive: true, force: true});
  }
});
