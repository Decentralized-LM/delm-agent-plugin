import assert from 'node:assert/strict';
import {mkdtemp, mkdir, readFile, rm, symlink, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {test} from 'node:test';
import {assertMaintenanceSafe, installationReadiness} from '../lib/maintenance.mjs';

const id = '11111111-2222-4333-8444-555555555555';
async function fixture(t) {
  const home = await mkdtemp(join(tmpdir(), 'delm maintenance '));
  t.after(() => rm(home, {recursive: true, force: true}));
  const run = join(home, 'Library/Application Support/DeLM/runs', id);
  await mkdir(run, {recursive: true});
  return {home, run};
}

test('maintenance does not create storage and accepts completed, cleaned runs', async t => {
  const {home, run} = await fixture(t);
  await assertMaintenanceSafe({home: join(home, 'absent'), host: 'codex'});
  for (const status of ['complete', 'delivered', 'stopped', 'delivery_conflict']) {
    await writeFile(join(run, 'run.json'), JSON.stringify({status}));
    await assertMaintenanceSafe({home, host: 'codex'});
  }
});

test('active and incomplete runs block only their host; unknown startup blocks both', async t => {
  const {home, run} = await fixture(t);
  for (const host of ['codex', 'claude']) await assert.rejects(assertMaintenanceSafe({home, host}), {code: 'ACTIVE_DELM_RUN'});
  await writeFile(join(run, 'claude.json'), JSON.stringify({status: 'running', finished: false}));
  await assertMaintenanceSafe({home, host: 'codex'});
  await assert.rejects(assertMaintenanceSafe({home, host: 'claude'}), error => error.code === 'ACTIVE_DELM_RUN' && error.message.includes('/delm-stop'));
  await writeFile(join(run, 'claude.json'), JSON.stringify({status: 'complete', finished: false}));
  await assert.rejects(assertMaintenanceSafe({home, host: 'claude'}), {code: 'ACTIVE_DELM_RUN'});
  await writeFile(join(run, 'claude.json'), JSON.stringify({status: 'complete', finished: true}));
  await assertMaintenanceSafe({home, host: 'claude'});
});

test('workspace remnants and recovery-required states block maintenance without deleting evidence', async t => {
  const {home, run} = await fixture(t);
  const file = join(run, 'run.json');
  await writeFile(file, JSON.stringify({status: 'recovery_required', task: 'private task'}));
  await assert.rejects(assertMaintenanceSafe({home, host: 'codex'}), error => error.code === 'ACTIVE_DELM_RUN' && !error.message.includes('private task'));
  await writeFile(file, JSON.stringify({status: 'complete'}));
  await mkdir(join(run, 'workspace/worker-1'), {recursive: true});
  await assert.rejects(assertMaintenanceSafe({home, host: 'codex'}), {code: 'ACTIVE_DELM_RUN'});
  assert.equal(await readFile(file, 'utf8'), '{"status":"complete"}');
});

test('invalid or linked records produce a safe actionable failure without revealing contents', async t => {
  const {home, run} = await fixture(t);
  await writeFile(join(run, 'run.json'), 'private invalid contents');
  await assert.rejects(assertMaintenanceSafe({home, host: 'codex'}), error => error.code === 'RUN_STATE_UNAVAILABLE' && !error.message.includes('private invalid'));
  await rm(join(run, 'run.json'));
  const external = join(home, 'external');
  await writeFile(external, '{"status":"complete"}');
  await symlink(external, join(run, 'run.json'));
  await assert.rejects(assertMaintenanceSafe({home, host: 'codex'}), {code: 'RUN_STATE_UNAVAILABLE'});
});

test('proven pre-launch failures permit maintenance without discarding their evidence', async t => {
  const {home, run} = await fixture(t);
  const codex = join(run, 'run.json');
  const failed = {host: 'codex', status: 'preparation_failed', native_started: false, finished: true, workspace_cleanup_complete: true};
  await writeFile(codex, JSON.stringify(failed));
  await assertMaintenanceSafe({home, host: 'codex'});
  for (const key of ['native_started', 'finished', 'workspace_cleanup_complete']) {
    const uncertain = {...failed};
    delete uncertain[key];
    await writeFile(codex, JSON.stringify(uncertain));
    await assert.rejects(assertMaintenanceSafe({home, host: 'codex'}), {code: 'ACTIVE_DELM_RUN'});
  }
  await rm(codex);
  for (const status of ['preparation_failed', 'startup_failed']) {
    await writeFile(join(run, 'claude.json'), JSON.stringify({status, finished: true}));
    await assertMaintenanceSafe({home, host: 'claude'});
  }
  await mkdir(join(run, 'workspace/capture-2'), {recursive: true});
  await assert.rejects(assertMaintenanceSafe({home, host: 'claude'}), {code: 'ACTIVE_DELM_RUN'});
});

test('conflicting host records cannot conceal active work from maintenance', async t => {
  const {home, run} = await fixture(t);
  await writeFile(join(run, 'claude.json'), JSON.stringify({status: 'complete', finished: true}));
  await writeFile(join(run, 'run.json'), JSON.stringify({status: 'running'}));
  for (const host of ['codex', 'claude']) await assert.rejects(assertMaintenanceSafe({home, host}), {code: 'RUN_STATE_UNAVAILABLE'});
});

test('status separates installed state from unverified session activation', () => {
  for (const host of ['codex', 'claude']) {
    const ready = installationReadiness(host, {installed: true, enabled: true});
    assert.equal(ready.installation, 'enabled');
    assert.equal(ready.session, 'not_checked');
    assert.ok(ready.nextSteps.some(step => step.includes(host === 'codex' ? '$delm:run' : '/delm:run')));
    assert.equal(installationReadiness(host, {installed: true, enabled: false}).installation, 'disabled');
    assert.equal(installationReadiness(host, {legacyInstalled: true}).installation, 'source_installation');
    assert.equal(installationReadiness(host, {conflict: true}).installation, 'needs_attention');
  }
});
