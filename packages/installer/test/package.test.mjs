import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {test} from 'node:test';

const root = fileURLToPath(new URL('..', import.meta.url));
const cli = path.join(root, 'bin/delm-agent.mjs');

test('package exposes help/version, rejects hidden repository overrides, and cannot publish accidentally', () => {
  const metadata = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8'));
  assert.equal(metadata.private, true);
  assert.deepEqual(metadata.dependencies ?? {}, {});
  assert.deepEqual(metadata.bin, {'delm-agent': 'bin/delm-agent.mjs'});
  assert.equal(metadata.repository, undefined);
  assert.equal(JSON.parse(readFileSync(path.join(root, 'release.json'), 'utf8')).repository, null);
  assert.equal(execFileSync(process.execPath, [cli, '--version'], {encoding: 'utf8'}).trim(), metadata.version);
  assert.match(execFileSync(process.execPath, [cli, '--help'], {encoding: 'utf8'}), /unpublished/);
  assert.throws(() => execFileSync(process.execPath, [cli, 'install', '--marketplace', '/tmp/foreign'], {stdio: 'pipe'}), error => error.status === 1);
  assert.throws(() => execFileSync(process.execPath, [cli, 'status', '--json'], {stdio: 'pipe'}), error => error.status === 1 && JSON.parse(error.stderr).code === 'UNCONFIGURED_RELEASE');
  for (const args of [['--host', 'other'], ['--host', 'claude', '--codex', 'codex'], ['--host', 'claude', '--claude', '']]) {
    assert.throws(() => execFileSync(process.execPath, [cli, 'status', ...args, '--json'], {stdio: 'pipe'}), error => error.status === 1 && JSON.parse(error.stderr).code === 'USAGE');
  }
});

test('npm pack contains only the runnable CLI, metadata, README and license', () => {
  const temporary = mkdtempSync(path.join(tmpdir(), 'delm npm pack '));
  try {
    const userconfig = path.join(temporary, 'user.npmrc');
    const globalconfig = path.join(temporary, 'global.npmrc');
    writeFileSync(userconfig, '');
    writeFileSync(globalconfig, '');
    const packed = JSON.parse(execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--offline', '--no-update-notifier', '--pack-destination', temporary,
      '--userconfig', userconfig, '--globalconfig', globalconfig, '--cache', path.join(temporary, 'cache')], {cwd: root, encoding: 'utf8'}));
    assert.deepEqual(packed[0].files.map(file => file.path).sort(), [
      'LICENSE', 'NOTICE', 'README.md', 'bin/delm-agent.mjs', 'lib/claude.mjs', 'lib/hosts.mjs', 'lib/installer.mjs', 'lib/maintenance.mjs', 'lib/native.mjs', 'package.json', 'release.json',
    ]);
    assert.ok(packed[0].size < 20_000, 'The thin installer should stay small.');
    assert.deepEqual(packed[0].bundled, []);
  } finally {
    rmSync(temporary, {recursive: true, force: true});
  }
});

test('preparation binds one destination, produces publishable metadata, and preserves source guards', () => {
  const temporary = mkdtempSync(path.join(tmpdir(), 'delm prepared installer '));
  const prepare = path.resolve(root, '../../scripts/prepare_installer.py');
  const sourceMetadata = readFileSync(path.join(root, 'package.json'));
  const sourceRelease = readFileSync(path.join(root, 'release.json'));
  const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
  try {
    const destination = 'delm-fixture/public-distribution';
    const output = path.join(temporary, 'prepared');
    const native = path.join(temporary, 'native-release.json');
    const record = {schema: 1, repository: destination, version: '0.3.0', sourceRevision: 'a'.repeat(40), runtimeSourcesSha256: 'b'.repeat(64)};
    writeFileSync(native, JSON.stringify(record));
    const args = [prepare, '--repository', destination, '--out', output, '--native-release', native];
    const preparation = JSON.parse(execFileSync('python3', args, {encoding: 'utf8'}));
    assert.equal(preparation.repository, destination);
    assert.equal(preparation.nativeRelease.sha256, sha256(readFileSync(native)));
    assert.equal(preparation.nativeRelease.version, '0.3.0');
    const metadata = JSON.parse(readFileSync(path.join(output, 'package/package.json'), 'utf8'));
    assert.equal(metadata.private, false);
    assert.equal(metadata.version, JSON.parse(sourceMetadata).version);
    assert.equal(metadata.repository.url, `git+https://github.com/${destination}.git`);
    assert.equal(metadata.scripts, undefined);
    const configured = JSON.parse(readFileSync(path.join(output, 'package/release.json'), 'utf8'));
    assert.deepEqual(configured, {...JSON.parse(sourceRelease), repository: destination});
    const help = execFileSync(process.execPath, [path.join(output, 'package/bin/delm-agent.mjs'), '--help'], {encoding: 'utf8'});
    assert.match(help, /Marketplace: https:\/\/github.com\/delm-fixture\/public-distribution/);
    assert.doesNotMatch(help, /unpublished|no release destination/);
    const readme = readFileSync(path.join(output, 'package/README.md'), 'utf8');
    assert.match(readme, /npx --yes delm-agent@latest install/);
    assert.match(readme, /npx --yes delm-agent@latest install --host claude/);
    assert.match(readme, /CLAUDE_CONFIG_DIR/);
    assert.match(help, /codex\|claude\|both/);
    assert.ok(readme.includes(destination));
    assert.doesNotMatch(readme, /jerry2247|unpublished|not an available install command/);
    for (const [relative, digest] of Object.entries(preparation.files)) {
      assert.equal(sha256(readFileSync(path.join(output, 'package', relative))), digest);
    }
    // Match the downloadable workflow artifact, which excludes staged package/.
    const download = path.join(temporary, 'download');
    mkdirSync(download);
    for (const file of [preparation.tarball.path, 'preparation.json', 'SHA256SUMS']) copyFileSync(path.join(output, file), path.join(download, file));
    const checksums = readFileSync(path.join(download, 'SHA256SUMS'), 'utf8').trim().split('\n');
    assert.equal(checksums.length, 2);
    for (const line of checksums) {
      const [digest, file] = line.split('  ');
      assert.equal(sha256(readFileSync(path.join(download, file))), digest);
    }
    const userconfig = path.join(temporary, 'user.npmrc');
    const globalconfig = path.join(temporary, 'global.npmrc');
    writeFileSync(userconfig, '');
    writeFileSync(globalconfig, '');
    const published = JSON.parse(execFileSync('npm', ['publish', path.join(output, preparation.tarball.path), '--dry-run', '--ignore-scripts', '--offline', '--json',
      '--userconfig', userconfig, '--globalconfig', globalconfig, '--cache', path.join(temporary, 'cache')], {encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe']}));
    // npm 10 returns package metadata directly; npm 11 keys it by package name.
    const publishedPackage = published['delm-agent'] ?? published;
    assert.equal(publishedPackage.name, 'delm-agent');
    assert.equal(publishedPackage.version, metadata.version);
    assert.throws(() => execFileSync('python3', args, {stdio: 'pipe'}), error => error.status === 1 && String(error.stderr).includes('already exists'));
    for (const repository of ['../invalid', 'https://github.com/owner/repository', 'other/destination']) {
      const invalidOutput = path.join(temporary, 'invalid');
      assert.throws(() => execFileSync('python3', [prepare, '--repository', repository, '--out', invalidOutput, '--native-release', native], {stdio: 'pipe'}), error => error.status === 1);
      assert.equal(existsSync(invalidOutput), false);
    }
    assert.deepEqual(readFileSync(path.join(root, 'package.json')), sourceMetadata);
    assert.deepEqual(readFileSync(path.join(root, 'release.json')), sourceRelease);
  } finally {
    rmSync(temporary, {recursive: true, force: true});
  }
});
