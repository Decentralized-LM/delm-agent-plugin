import {constants} from 'node:fs';
import {lstat, open, readdir} from 'node:fs/promises';
import {homedir} from 'node:os';
import {join} from 'node:path';
import {InstallerError} from './native.mjs';

const RUN_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const TERMINAL = new Set(['complete', 'delivered', 'stopped', 'delivery_conflict']);

async function metadata(path) {
  try { return await lstat(path); }
  catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}

async function record(path) {
  let file;
  try {
    file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  } catch (error) { if (error.code === 'ENOENT') return null; throw error; }
  try {
    const info = await file.stat();
    if (!info.isFile() || info.size > 16 * 1024 * 1024) throw new Error('Unsupported run record');
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
      if (!value || !(TERMINAL.has(value.status) || failedBeforeStart) || (claude && claude.finished !== true) || remaining) {
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
