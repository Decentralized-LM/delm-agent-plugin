import {constants} from 'node:fs';
import {createHash} from 'node:crypto';
import {lstat, open, readdir} from 'node:fs/promises';
import {homedir} from 'node:os';
import {join} from 'node:path';
import {InstallerError} from './native.mjs';

const RUN_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const TERMINAL = new Set(['complete', 'delivered', 'stopped', 'delivery_conflict']);

// Recovery is durable only when every referenced byte is still present. A
// status string or an empty recovery directory is not evidence of saved work.
async function verifiedRecovery(path) {
  const root = join(path, 'workspace', 'recovery');
  if (!(await metadata(root))?.isDirectory()) return false;
  const bundle = await record(join(root, 'complete.json'), 64 * 1024 * 1024);
  if (bundle?.version !== 1 || typeof bundle.original !== 'string' || !bundle.original.startsWith('/') || !Array.isArray(bundle.workers) || bundle.workers.length > 2) return false;
  const verified = new Set();
  const workers = new Set();
  let entries = 0;
  for (const worker of bundle.workers) {
    if (!worker || ![0, 1].includes(worker.worker) || workers.has(worker.worker) || !worker.changes || typeof worker.changes !== 'object' || Array.isArray(worker.changes)) return false;
    workers.add(worker.worker);
    for (const [relative, pair] of Object.entries(worker.changes)) {
      const parts = relative.split('/');
      if (++entries > 1000000 || relative.includes('\0') || parts.length > 128 || parts.some(part => !part || part === '.' || part === '..' || part.toLowerCase() === '.git') || !Array.isArray(pair) || pair.length !== 2 || pair.every(value => value === null)) return false;
      for (const value of pair) {
        if (value === null) continue;
        if (!value || !['file', 'directory', 'symlink'].includes(value.kind)
          || !Number.isSafeInteger(value.mode) || value.mode < 0 || value.mode > 0o7777
          || !Number.isSafeInteger(value.size) || value.size < 0
          || typeof value.xattrs_sha256 !== 'string' || typeof value.acl_sha256 !== 'string'
          || !Number.isSafeInteger(value.xattrs_bytes) || value.xattrs_bytes < 0
          || !Number.isSafeInteger(value.flags) || value.flags < 0 || value.flags > 0xffffffff
          || value.sha256 != null && typeof value.sha256 !== 'string'
          || value.link_target != null && typeof value.link_target !== 'string'
          || value.kind === 'symlink' && typeof value.link_target !== 'string') return false;
        if (value.kind !== 'file') continue;
        if (!/^[a-f0-9]{64}$/.test(value.sha256) || !Number.isSafeInteger(value.size) || value.size < 0) return false;
        const identity = `${value.sha256}:${value.size}`;
        if (verified.has(identity)) continue;
        const file = await open(join(root, value.sha256), constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
        try {
          const info = await file.stat();
          if (!info.isFile() || info.size !== value.size) return false;
          const hash = createHash('sha256');
          for await (const chunk of file.createReadStream({autoClose: false})) hash.update(chunk);
          if (hash.digest('hex') !== value.sha256) return false;
        } finally { await file.close(); }
        verified.add(identity);
      }
    }
  }
  return true;
}

async function metadata(path) {
  try { return await lstat(path); }
  catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}

async function record(path, maxBytes = 16 * 1024 * 1024) {
  let file;
  try {
    file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  } catch (error) { if (error.code === 'ENOENT') return null; throw error; }
  try {
    const info = await file.stat();
    if (!info.isFile() || info.size > maxBytes) throw new Error('Unsupported run record');
    const value = JSON.parse(await file.readFile('utf8'));
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid run record');
    return value;
  } finally { await file.close(); }
}

// This preflight protects existing runs. It is not a lock against a new run
// starting in another host after the check; users must stop work for maintenance.
export async function assertMaintenanceSafe({host, home = homedir()} = {}) {
  const root = join(home, 'Library', 'Application Support', 'DeLM');
  try {
    const rootInfo = await metadata(root);
    if (!rootInfo) return;
    if (!rootInfo.isDirectory()) throw new Error('DeLM storage is not a directory');
    const runs = join(root, 'runs');
    const runsInfo = await metadata(runs);
    if (!runsInfo) return;
    if (!runsInfo.isDirectory()) throw new Error('DeLM run storage is not a directory');
    const entries = await readdir(runs, {withFileTypes: true});
    if (entries.length > 16384) throw new Error('Too many runs to inspect safely');
    for (const entry of entries) {
      if (!RUN_ID.test(entry.name)) continue;
      if (!entry.isDirectory()) throw new Error('Run storage contains an unexpected entry');
      const path = join(runs, entry.name);
      const claude = await record(join(path, 'claude.json'));
      const codex = await record(join(path, 'run.json'));
      if (claude && codex) throw new Error('Conflicting native host records');
      if (claude && host === 'codex' || codex && host === 'claude') continue;
      const value = claude ?? codex;
      const workspace = join(path, 'workspace');
      const workspaceInfo = await metadata(workspace);
      if (workspaceInfo && !workspaceInfo.isDirectory()) throw new Error('Unexpected workspace entry');
      const remaining = workspaceInfo && (await readdir(workspace, {withFileTypes: true}))
        .some(item => item.isSymbolicLink() || item.isDirectory() && !['delivery', 'recovery'].includes(item.name));
      const failedBeforeStart = value && ['preparation_failed', 'startup_failed'].includes(value.status)
        && value.finished === true
        && (claude || value.native_started === false && value.workspace_cleanup_complete === true);
      const shutdownUnconfirmed = claude?.finalization != null && claude.finalization.shutdown_ack !== 'confirmed';
      let savedRecovery = false;
      if (value?.status === 'recovery_required' && !remaining) {
        const shutdown = codex && await record(join(path, 'shutdown-report.json'));
        const quiet = claude ? claude.finished === true
          : shutdown?.ownership_resolved === true && Array.isArray(shutdown.survivors) && !shutdown.survivors.length && Array.isArray(shutdown.errors) && !shutdown.errors.length;
        savedRecovery = quiet && await verifiedRecovery(path);
      }
      if (!value || !(TERMINAL.has(value.status) || failedBeforeStart || savedRecovery) || (claude && claude.finished !== true) || shutdownUnconfirmed || remaining) {
        const action = !value || ['preparation_failed', 'startup_failed', 'recovery_required'].includes(value.status)
          ? 'Inspect this run using the run-management commands in the DeLM support guide and resolve its recovery state'
          : claude ? 'Use /delm-stop in its Claude conversation' : 'Stop DeLM in its owning conversation';
        throw new InstallerError(`DeLM run ${entry.name} is active or still needs recovery. ${action}, wait for confirmed shutdown and workspace cleanup, then retry. Saved work was not changed.`, 'ACTIVE_DELM_RUN');
      }
    }
  } catch (error) {
    if (error instanceof InstallerError) throw error;
    throw new InstallerError('Could not safely inspect DeLM run storage. Resolve its permissions or recovery state before updating or removing the plugin. Saved work was not changed.', 'RUN_STATE_UNAVAILABLE');
  }
}

export function installationReadiness(host, state) {
  const installation = state.conflict || state.scopeConflict ? 'needs_attention'
    : state.installed ? state.enabled ? 'enabled' : 'disabled'
      : state.legacyInstalled ? 'source_installation' : 'not_installed';
  const nextSteps = [];
  if (installation === 'needs_attention') nextSteps.push('Resolve the native plugin registration reported by status.');
  else if (installation === 'disabled') nextSteps.push('Run install to enable the plugin.');
  else if (installation === 'not_installed') nextSteps.push('Run install to add the plugin.');
  else if (host === 'codex') nextSteps.push('Restart Codex, review DeLM in /hooks, and restart after granting trust.', 'Use $delm:run <task> in your project.');
  else nextSteps.push('Restart Claude Code and review its native trust or permission prompts.', 'Use /delm:run <task> in your project.');
  return {installation, session: 'not_checked', nextSteps};
}
