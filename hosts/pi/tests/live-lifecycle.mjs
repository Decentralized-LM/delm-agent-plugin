#!/usr/bin/env node
// Account-backed lifecycle checks against the installed Pi and a real model.
// Not part of `npm test`: each scenario starts real workers and bills the
// provider login Pi is using. Run explicitly:
//
//   node hosts/pi/tests/live-lifecycle.mjs update|stop|recover|all [--package DIR] [--model provider/id]
//
// Evidence goes to .validation/pi-live/<scenario>-<timestamp>/.
import {spawn, execFileSync} from 'node:child_process';
import {existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync, readdirSync, realpathSync, statSync} from 'node:fs';
import {tmpdir, homedir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, '../../..');
const args = process.argv.slice(2);
const packageIndex = args.indexOf('--package');
const packageDir = packageIndex >= 0 ? resolve(args[packageIndex + 1]) : join(ROOT, '.build', 'plugin-pi');
// `--model provider/id` picks the parent's (and so the workers') model; a cheap one validates the mechanics.
const modelIndex = args.indexOf('--model');
const modelArgs = modelIndex >= 0 ? ['--model', args[modelIndex + 1]] : [];
const valueSlots = new Set([packageIndex, modelIndex].filter(i => i >= 0).map(i => i + 1));
const positional = args.filter((a, i) => !a.startsWith('--') && !valueSlots.has(i));
const scenario = positional[0] || 'all';
const runtime = join(packageDir, 'bin', 'delm');
const extension = join(packageDir, 'extensions', 'delm.ts');
if (!existsSync(runtime) || !existsSync(extension)) throw new Error('Build the Pi package first: ./scripts/build.sh --host pi');
const evidenceRoot = join(ROOT, '.validation', 'pi-live');
mkdirSync(evidenceRoot, {recursive: true});

const sleep = ms => new Promise(r => setTimeout(r, ms));
const git = (dir, ...a) => execFileSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@example.com', ...a], {cwd: dir, stdio: 'pipe', encoding: 'utf8'});

function project() {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'delm-pi-live-')));
  writeFileSync(join(dir, 'README.md'), '# live fixture\n');
  git(dir, 'init', '-q'); git(dir, 'add', '.'); git(dir, 'commit', '-q', '-m', 'init');
  return dir;
}

function parent(dir, evidence) {
  const child = spawn('pi', ['--mode', 'rpc', '--no-session', ...modelArgs, '-e', extension], {
    cwd: dir, stdio: ['pipe', 'pipe', 'pipe'], env: {...process.env, DELM_PI_RUNTIME: runtime, DELM_PI_WORKER: ''},
  });
  const records = [];
  const notices = [];
  let buffer = '';
  child.stdout.setEncoding('utf8');
  child.stdout.on('data', chunk => {
    buffer += chunk;
    const lines = buffer.split('\n'); buffer = lines.pop();
    for (const line of lines) {
      if (!line.trim()) continue;
      let record; try { record = JSON.parse(line); } catch { continue; }
      records.push(record);
      if (record.type === 'extension_ui_request' && record.method === 'notify') {
        notices.push(record.message);
        console.log('  [notice] ' + record.message.slice(0, 200));
      }
    }
  });
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', chunk => { stderr += chunk; });
  const send = command => child.stdin.write(JSON.stringify(command) + '\n');
  const waitFor = async (pattern, timeoutMs) => {
    const started = Date.now();
    while (Date.now() - started < timeoutMs) {
      const hit = notices.find(n => pattern.test(n));
      if (hit) return hit;
      if (child.exitCode !== null) return null;
      await sleep(500);
    }
    return null;
  };
  const finish = () => {
    writeFileSync(join(evidence, 'parent-records.jsonl'), records.map(r => JSON.stringify(r)).join('\n') + '\n');
    writeFileSync(join(evidence, 'parent-stderr.txt'), stderr);
  };
  return {child, send, waitFor, notices, finish};
}

// Pi's RPC processes retitle themselves, so argv is useless; the parent logs every worker pid.
function strayWorkers(since) {
  const alive = [];
  const sessions = readdirSync(tmpdir()).filter(name => name.startsWith('delm-pi-sessions-')).map(name => join(tmpdir(), name))
    .filter(dir => { try { return statSync(dir).mtimeMs >= since; } catch { return false; } });
  for (const dir of sessions) {
    let log = '';
    try { log = readFileSync(join(dir, 'delm-pi.log'), 'utf8'); } catch { continue; }
    for (const match of log.matchAll(/(worker \d|runtime) pid (\d+)/g)) {
      const pid = Number(match[2]);
      if (pid <= 1) continue;
      try { process.kill(pid, 0); alive.push(match[1] + ' pid ' + pid + ' (' + dir + ')'); } catch { /* exited */ }
    }
  }
  return alive;
}

function unfinishedRuns(dir) {
  const root = join(homedir(), 'Library', 'Application Support', 'DeLM', 'runs');
  return readdirSync(root).filter(id => {
    try { const s = JSON.parse(readFileSync(join(root, id, 'claude.json'), 'utf8')); return s.host === 'pi' && !s.finished && s.project === dir; }
    catch { return false; }
  });
}

const TASK = 'Add hello.py with a greet(name) function that returns "Hello, <name>!" and a pytest test for it. Keep it minimal.';
const results = {};

const stamp = () => new Date().toISOString().replace(/[:.]/g, '-');
const evidenceDir = name => { const dir = join(evidenceRoot, name + '-' + stamp()); mkdirSync(dir, {recursive: true}); return dir; };
const stopParent = async p => { try { p.child.stdin.end(); } catch { /* closed */ } await sleep(3000); if (p.child.exitCode === null) p.child.kill('SIGKILL'); p.finish(); };

async function scenarioUpdate() {
  const dir = project(); const evidence = evidenceDir('update'); const since = Date.now();
  console.log('update: project ' + dir);
  const p = parent(dir, evidence);
  try {
    p.send({id: 'run', type: 'prompt', message: '/delm:run ' + TASK});
    const started = await p.waitFor(/started two peers/, 60000);
    if (!started) throw new Error('run did not start: ' + p.notices.join(' | '));
    await sleep(20000);
    p.send({id: 'update', type: 'prompt', message: 'Also add a farewell(name) function returning "Goodbye, <name>!" with its own test.'});
    const forwarded = await p.waitFor(/forwarded your update/, 30000);
    const final = await p.waitFor(/delivered the result|DeLM stopped|run interrupted|could not start/, 600000);
    await sleep(2000);
    const hello = existsSync(join(dir, 'hello.py')) ? readFileSync(join(dir, 'hello.py'), 'utf8') : '';
    results.update = {forwarded: Boolean(forwarded), final, farewellDelivered: /farewell/.test(hello),
      errors: p.notices.filter(n => /^DeLM: /.test(n)), evidence};
  } finally {
    await stopParent(p);
    if (results.update) results.update.strays = strayWorkers(since);
    console.log('update:', JSON.stringify(results.update, null, 1));
  }
}

async function scenarioStop() {
  const dir = project(); const evidence = evidenceDir('stop'); const since = Date.now();
  console.log('stop: project ' + dir);
  const p = parent(dir, evidence);
  try {
    p.send({id: 'run', type: 'prompt', message: '/delm:run ' + TASK});
    if (!await p.waitFor(/started two peers/, 60000)) throw new Error('run did not start: ' + p.notices.join(' | '));
    await sleep(15000);
    p.send({id: 'stop', type: 'prompt', message: '/delm:stop'});
    const final = await p.waitFor(/stopped|Stopped|saved|delivered|interrupted/, 180000);
    await sleep(2000);
    results.stop = {final, projectStatus: git(dir, 'status', '--short').trim(), unfinished: unfinishedRuns(dir), evidence};
  } finally {
    await stopParent(p);
    if (results.stop) results.stop.strays = strayWorkers(since);
    console.log('stop:', JSON.stringify(results.stop, null, 1));
  }
}

async function scenarioRecover() {
  const dir = project(); const evidence = evidenceDir('recover'); const since = Date.now();
  console.log('recover: project ' + dir);
  const p = parent(dir, evidence);
  let q = null;
  try {
    p.send({id: 'run', type: 'prompt', message: '/delm:run ' + TASK});
    if (!await p.waitFor(/started two peers/, 60000)) throw new Error('run did not start: ' + p.notices.join(' | '));
    await sleep(15000);
    p.child.kill('SIGKILL');            // the session dies without any shutdown handler
    await sleep(8000); p.finish();
    const straysAfterKill = strayWorkers(since);
    const unfinishedBefore = unfinishedRuns(dir);
    q = parent(dir, join(evidence, 'second-session'));
    mkdirSync(join(evidence, 'second-session'), {recursive: true});
    const warned = await q.waitFor(/unfinished DeLM run owns this project/, 20000);
    q.send({id: 'recover', type: 'prompt', message: '/delm:recover'});
    const recovered = await q.waitFor(/recovered|could not recover/, 180000);
    await sleep(1000);
    results.recover = {straysAfterKill, unfinishedBefore, warned: Boolean(warned), recovered, unfinishedAfter: unfinishedRuns(dir),
      projectStatus: git(dir, 'status', '--short').trim(), evidence};
  } finally {
    if (p.child.exitCode === null) p.child.kill('SIGKILL');
    if (q) await stopParent(q);
    console.log('recover:', JSON.stringify(results.recover, null, 1));
  }
}

const scenarios = {update: scenarioUpdate, stop: scenarioStop, recover: scenarioRecover};
try {
  for (const name of scenario === 'all' ? Object.keys(scenarios) : [scenario]) {
    if (!scenarios[name]) throw new Error('Unknown scenario ' + name);
    await scenarios[name]();
  }
} finally {
  writeFileSync(join(evidenceRoot, 'latest.json'), JSON.stringify(results, null, 2));
}
console.log('evidence: ' + evidenceRoot);
