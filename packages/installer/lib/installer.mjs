import {execFile} from 'node:child_process';
import {mkdtemp, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {promisify} from 'node:util';

const execFileAsync = promisify(execFile);
export const RELEASE = Object.freeze({
  repository: 'jerry2247/delm-agent-plugin',
  url: 'https://github.com/jerry2247/delm-agent-plugin.git',
  ref: 'marketplace',
  marketplace: 'delm',
  plugin: 'delm@delm',
});
export const COMMANDS = ['install', 'update', 'remove', 'status'];

export class InstallerError extends Error {
  constructor(message, code = 'INSTALLER_ERROR') {
    super(message);
    this.name = 'InstallerError';
    this.code = code;
  }
}

export async function execute(file, args, options = {}) {
  try {
    return await execFileAsync(file, args, {
      env: process.env, encoding: 'utf8', timeout: 120_000,
      maxBuffer: 4 * 1024 * 1024, ...options,
    });
  } catch (error) {
    if (error.code === 'ENOENT') {
      throw new InstallerError(`Cannot find ${file}. Install it and make it available on PATH.`, 'MISSING_EXECUTABLE');
    }
    const detail = String(error.stderr || error.stdout || error.message).trim();
    throw new InstallerError(`${file} ${args.join(' ')} failed: ${detail}`, 'NATIVE_COMMAND_FAILED');
  }
}

function collection(value, key) {
  if (!value || !Array.isArray(value[key])) {
    throw new InstallerError(`Codex returned an unsupported ${key} response. Update stock Codex CLI and retry.`, 'UNSUPPORTED_CODEX');
  }
  return value[key];
}

// Distribution identity is fixed. Tests redirect this Git URL through a disposable
// Git configuration; there is no production repository or marketplace override.
export async function manage(command, {codex = 'codex', platform = process.platform, run = execute} = {}) {
  if (codex.includes('/') || codex.includes('\\')) codex = resolve(codex);
  if (!COMMANDS.includes(command)) throw new InstallerError(`Unknown command: ${command}`, 'USAGE');
  if (command !== 'status' && platform !== 'darwin') {
    throw new InstallerError('DeLM installation currently supports macOS only. Windows and Linux support is not released yet.', 'UNSUPPORTED_PLATFORM');
  }
  const cwd = await mkdtemp(join(tmpdir(), 'delm-installer-'));
  try {
    return await manageNative(command, {codex, run: (file, args) => run(file, args, {cwd})});
  } finally {
    await rm(cwd, {recursive: true, force: true});
  }
}

async function manageNative(command, {codex, run}) {
  const native = async (...args) => {
    let result;
    try {
      result = await run(codex, ['plugin', ...args, '--json']);
    } catch (error) {
      if (error.code === 'MISSING_EXECUTABLE') {
        throw new InstallerError('Install stock Codex CLI and make `codex` available on PATH, or pass --codex PATH. A desktop or IDE installation alone is insufficient.', error.code);
      }
      throw error;
    }
    try {
      return JSON.parse(result.stdout);
    } catch {
      throw new InstallerError('Codex did not return native plugin JSON. Update stock Codex CLI and retry.', 'UNSUPPORTED_CODEX');
    }
  };
  const readState = async () => {
    const marketplaces = collection(await native('marketplace', 'list'), 'marketplaces');
    const installed = collection(await native('list', '--marketplace', RELEASE.marketplace), 'installed');
    const legacy = collection(await native('list', '--marketplace', 'delm-local'), 'installed');
    const matching = marketplaces.filter(item => item.name === RELEASE.marketplace);
    const marketplace = matching[0];
    const source = marketplace?.marketplaceSource;
    const expectedSource = source?.sourceType === 'git'
      && [RELEASE.url, RELEASE.url.slice(0, -4)].includes(source.source);
    const conflict = matching.length > 1 || (marketplace && !expectedSource);
    const plugins = installed.filter(item => item.pluginId === RELEASE.plugin);
    if (plugins.length > 1) throw new InstallerError('Codex reports multiple DeLM installations. Resolve them in the native plugin manager before continuing.', 'AMBIGUOUS_INSTALLATION');
    const plugin = plugins[0];
    return {
      plugin: RELEASE.plugin,
      marketplaceRegistered: Boolean(marketplace),
      installed: Boolean(plugin?.installed),
      enabled: Boolean(plugin?.enabled),
      version: plugin?.version ?? null,
      legacyInstalled: legacy.some(item => item.pluginId === 'delm@delm-local' && item.installed),
      conflict: Boolean(conflict),
    };
  };
  const before = await readState();
  if (command === 'status') return {command, ...before};
  if (before.conflict) {
    throw new InstallerError('The marketplace name delm belongs to a different or unidentifiable source. It was preserved. Review `codex plugin marketplace list --json` before continuing.', 'MARKETPLACE_CONFLICT');
  }
  if (before.legacyInstalled && command !== 'remove') {
    throw new InstallerError('An existing delm-local source installation was preserved. Stop active DeLM work, run ./scripts/uninstall.sh from its original checkout, then retry. If removal refuses modified files, follow the documented native migration path to preserve them.', 'LEGACY_INSTALLATION');
  }
  if (before.installed && !before.marketplaceRegistered) {
    throw new InstallerError('DeLM is installed without an identifiable marketplace registration. Review its native registration before continuing.', 'MARKETPLACE_CONFLICT');
  }
  if (command === 'remove') {
    if (before.installed) {
      const result = await native('remove', RELEASE.plugin);
      if (result.pluginId !== RELEASE.plugin) throw new InstallerError('Codex returned an unexpected plugin identity.', 'UNEXPECTED_IDENTITY');
    }
    const after = await readState();
    if (after.installed) throw new InstallerError('Codex still reports DeLM installed after removal.', 'VERIFICATION_FAILED');
    return {command, changed: before.installed, ...after};
  }
  if (command === 'update' && !before.installed) {
    throw new InstallerError('DeLM is not installed. Run `npx --yes delm-agent@latest install` first once the package is published.', 'NOT_INSTALLED');
  }
  await run('git', ['--version']);
  // Native add checks the configured ref as well as the URL. Its own conflict
  // handling preserves an existing marketplace registered against another ref.
  const marketplace = await native('marketplace', 'add', RELEASE.url, '--ref', RELEASE.ref);
  if (marketplace.marketplaceName !== RELEASE.marketplace) {
    throw new InstallerError('The repository returned an unexpected marketplace identity. No plugin was installed; review the native marketplace registration.', 'UNEXPECTED_IDENTITY');
  }
  let changed = !before.installed || !before.enabled;
  if (command === 'update') {
    const outcome = await native('marketplace', 'upgrade', RELEASE.marketplace);
    if (Array.isArray(outcome.errors) && outcome.errors.length) {
      throw new InstallerError('Codex reported a marketplace upgrade failure; inspect its native plugin state before retrying.', 'VERIFICATION_FAILED');
    }
    changed = true;
  } else if (!before.installed || !before.enabled) {
    const result = await native('add', RELEASE.plugin);
    if (result.pluginId !== RELEASE.plugin) throw new InstallerError('Codex returned an unexpected plugin identity.', 'UNEXPECTED_IDENTITY');
  }
  const after = await readState();
  if (!after.marketplaceRegistered || !after.installed || (command === 'install' && !after.enabled) || after.conflict || after.legacyInstalled) {
    throw new InstallerError('The expected DeLM installation could not be verified. Review `codex plugin list --marketplace delm --json` before retrying.', 'VERIFICATION_FAILED');
  }
  return {command, changed, ...after};
}

export function describe(result) {
  const label = result.version ? `DeLM ${result.version}` : 'DeLM';
  if (result.command === 'status') {
    const lines = result.installed
      ? [`${label} (delm@delm) is ${result.enabled ? 'enabled' : 'disabled'} in Codex.`]
      : result.legacyInstalled
        ? ['DeLM is installed from source (delm@delm-local).', 'The public plugin (delm@delm) is not installed.']
        : ['DeLM is not installed.'];
    if (result.installed && result.legacyInstalled) lines.push('A source installation (delm@delm-local) is also present.');
    if (result.conflict) lines.push('The delm marketplace has a different or unidentifiable source. Review `codex plugin marketplace list --json`.');
    return lines.join('\n');
  }
  if (result.command === 'remove') {
    const lines = [result.changed
      ? 'Removed DeLM (delm@delm). Saved runs, accounts, and the marketplace registration were retained.'
      : 'The public plugin (delm@delm) is not installed. Nothing changed.'];
    if (result.legacyInstalled) lines.push('The source installation (delm@delm-local) remains installed.');
    return lines.join('\n');
  }
  if (result.command === 'install' && !result.changed) return [
    `${label} is already installed and enabled. No reinstall was needed.`,
    'Use $delm:run <task>. If DeLM is unavailable in your session, restart Codex, review its hooks in /hooks, and restart after granting trust.',
  ].join('\n');
  if (result.command === 'update' && !result.enabled) return [
    `Updated ${label} through Codex.`,
    'DeLM remains disabled. Use the install command to enable it before running a task.',
  ].join('\n');
  return [
    result.command === 'update' ? `Updated ${label} through Codex.`
      : `Installed ${label} in Codex.`,
    'Restart Codex, open /hooks, and review and trust the DeLM hooks if requested.',
    'Restart once more after granting trust, then use $delm:run <task>.',
    'Installation does not grant hook trust or start workers.',
  ].join('\n');
}
