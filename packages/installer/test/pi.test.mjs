import assert from 'node:assert/strict';
import {test} from 'node:test';
import {InstallerError, RELEASE as SOURCE_RELEASE, describe, manage} from '../lib/installer.mjs';
import {parsePackageList, piSource} from '../lib/pi.mjs';

const RELEASE = {...SOURCE_RELEASE, repository: 'delm-fixture/distribution', url: 'https://github.com/delm-fixture/distribution.git'};
const SOURCE = piSource(RELEASE);

// A scripted `pi` whose package list is the only state it keeps.
function fake({installed = false, variant = null, project = null, version = '1.5.0'} = {}) {
  const state = {user: ['./other-package', ...(installed ? [SOURCE] : []), ...(variant ? [variant] : [])], project: project ? [project] : [], calls: [], intercept: null};
  const listing = () => ['User packages:', ...state.user.flatMap(source => [`  ${source}`, `    /resolved/${source.replace(/[^a-z0-9]+/gi, '-')}`]),
    ...(state.project.length ? ['', 'Project packages:', ...state.project.flatMap(source => [`  ${source}`, `    /resolved/project`])] : [])].join('\n') + '\n';
  const run = async (file, args) => {
    state.calls.push([file, ...args]);
    const intercepted = await state.intercept?.(file, args);
    if (intercepted) return intercepted;
    if (file === 'git') return {stdout: 'git version fixture\n'};
    assert.equal(file, '/existing pi');
    if (args.length === 1 && args[0] === '--version') return {stdout: `${version}\n`};
    if (args.length === 1 && args[0] === 'list') return {stdout: listing()};
    if (args[0] === 'install') {
      assert.deepEqual(args, ['install', SOURCE]);
      state.user.push(SOURCE);
      return {stdout: `Installed ${SOURCE}\n`};
    }
    if (args[0] === 'remove') {
      assert.deepEqual(args, ['remove', SOURCE]);
      state.user = state.user.filter(source => source !== SOURCE);
      return {stdout: `Removed ${SOURCE}\n`};
    }
    if (args[0] === 'update') {
      assert.deepEqual(args, ['update', '--extension', SOURCE]);
      return {stdout: `Updated ${SOURCE}\n`};
    }
    assert.fail(`Unexpected pi invocation ${args.join(' ')}`);
  };
  return {state, run};
}

const options = (fixture, extra = {}) => ({host: 'pi', pi: '/existing pi', platform: 'darwin', release: RELEASE, run: fixture.run,
  checkMaintenance: async ({host}) => { assert.equal(host, 'pi'); fixture.state.maintenanceChecks = (fixture.state.maintenanceChecks ?? 0) + 1; }, ...extra});
const mutations = state => state.calls.filter(call => call[0] === '/existing pi' && !['list', '--version'].includes(call[1]));

test('the package list parser reads user and project sections and ignores resolved paths', () => {
  const listed = parsePackageList('User packages:\n  ./local\n    /Users/me/local\n  git:github.com/a/b@marketplace\n    /Users/me/.pi/agent/git/a-b\n\nProject packages:\n  npm:@x/y\n    /p/node_modules/@x/y\n');
  assert.deepEqual(listed, {user: ['./local', 'git:github.com/a/b@marketplace'], project: ['npm:@x/y']});
  assert.deepEqual(parsePackageList('No packages installed.\n'), {user: [], project: []});
});

test('install registers the marketplace source once, verifies it, and is idempotent', async () => {
  const fixture = fake();
  const result = await manage('install', options(fixture));
  assert.equal(result.host, 'pi');
  assert.equal(result.changed, true);
  assert.equal(result.installed, true);
  assert.deepEqual(mutations(fixture.state), [['/existing pi', 'install', SOURCE]]);
  assert.equal(fixture.state.maintenanceChecks, 1);
  assert.deepEqual(result.readiness.nextSteps, ['Start a new Pi session to load the plugin.', 'Use /delm:run <task> in your project.']);
  assert.match(describe(result), /Installed DeLM in Pi\.[\s\S]*\/delm:run/);
  const repeated = await manage('install', options(fixture));
  assert.equal(repeated.changed, false);
  assert.equal(mutations(fixture.state).length, 1);
  assert.match(describe(repeated), /already installed in Pi/);
});

test('status, update and remove use the registered source and report their outcome', async () => {
  const fixture = fake({installed: true});
  const status = await manage('status', options(fixture));
  assert.equal(status.installed, true);
  assert.equal(mutations(fixture.state).length, 0);
  assert.match(describe(status), /installed in Pi from git:github\.com\/delm-fixture\/distribution@marketplace/);
  const updated = await manage('update', options(fixture));
  assert.equal(updated.changed, true);
  assert.deepEqual(mutations(fixture.state), [['/existing pi', 'update', '--extension', SOURCE]]);
  assert.match(describe(updated), /Updated DeLM through Pi/);
  const removed = await manage('remove', options(fixture));
  assert.equal(removed.changed, true);
  assert.equal(removed.installed, false);
  assert.match(describe(removed), /Removed DeLM \(delm-pi\) from Pi/);
  const again = await manage('remove', options(fixture));
  assert.equal(again.changed, false);
  assert.match(describe(again), /not installed in Pi\. Nothing changed/);
});

test('update requires an installation and old or foreign executables are refused before any change', async () => {
  await assert.rejects(manage('update', options(fake())), {code: 'NOT_INSTALLED'});
  const old = fake({version: '1.3.9'});
  await assert.rejects(manage('install', options(old)), {code: 'UNSUPPORTED_PI_VERSION'});
  assert.equal(mutations(old.state).length, 0);
  const foreign = fake({version: 'codex-cli 0.160.1'});
  await assert.rejects(manage('install', options(foreign)), {code: 'INVALID_PI_EXECUTABLE'});
  const missing = fake();
  missing.state.intercept = async () => { const error = new InstallerError('missing', 'MISSING_EXECUTABLE'); throw error; };
  await assert.rejects(manage('install', options(missing)), error => error.code === 'MISSING_EXECUTABLE' && error.message.includes('--pi PATH'));
});

test('another registration of the repository or a project-scoped one is preserved and blocks changes', async () => {
  const variant = fake({variant: 'https://github.com/delm-fixture/distribution'});
  await assert.rejects(manage('install', options(variant)), {code: 'MARKETPLACE_CONFLICT'});
  assert.equal(mutations(variant.state).length, 0);
  const status = await manage('status', options(variant));
  assert.equal(status.conflict, true);
  assert.match(describe(status), /another source or reference/);
  const project = fake({project: SOURCE});
  await assert.rejects(manage('install', options(project)), {code: 'SCOPE_CONFLICT'});
  assert.equal(mutations(project.state).length, 0);
});

test('a previously approved repository address keeps its installation recognised', async () => {
  const release = {...RELEASE, previousRepositories: ['old-owner/old-repo']};
  const fixture = fake();
  fixture.state.user.push(piSource(release, 'old-owner/old-repo'));
  const status = await manage('status', options(fixture, {release}));
  assert.equal(status.installed, true);
  assert.equal(status.conflict, false);
  assert.equal(status.source, 'git:github.com/old-owner/old-repo@marketplace');
});

test('verification failures after a mutation are reported, not hidden', async () => {
  const fixture = fake();
  fixture.state.intercept = async (file, args) => (file === '/existing pi' && args[0] === 'install') ? {stdout: 'pretended\n'} : null;
  await assert.rejects(manage('install', options(fixture)), {code: 'VERIFICATION_FAILED'});
});
