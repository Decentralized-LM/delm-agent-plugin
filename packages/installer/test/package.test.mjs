import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
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
  assert.equal(execFileSync(process.execPath, [cli, '--version'], {encoding: 'utf8'}).trim(), metadata.version);
  assert.match(execFileSync(process.execPath, [cli, '--help'], {encoding: 'utf8'}), /unpublished/);
  assert.throws(() => execFileSync(process.execPath, [cli, 'install', '--marketplace', '/tmp/foreign'], {stdio: 'pipe'}), error => error.status === 1);
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
      'LICENSE', 'NOTICE', 'README.md', 'bin/delm-agent.mjs', 'lib/installer.mjs', 'package.json',
    ]);
    assert.ok(packed[0].size < 20_000, 'The thin installer should stay small.');
    assert.deepEqual(packed[0].bundled, []);
  } finally {
    rmSync(temporary, {recursive: true, force: true});
  }
});
