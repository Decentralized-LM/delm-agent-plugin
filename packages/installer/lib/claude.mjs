import {InstallerError} from './native.mjs';

function unsupported() {
  return new InstallerError('Claude Code returned unsupported native plugin JSON. Update Claude Code and retry.', 'UNSUPPORTED_CLAUDE');
}

// Read operations return a complete JSON document. Mutation operations may
// print progress first; their contract places one result on the final line.
function parse(stdout, mutation) {
  try {
    const text = stdout.trim();
    return JSON.parse(mutation ? text.split(/\r?\n/).at(-1) : text);
  } catch {
    throw unsupported();
  }
}

function checkIdentity(result, {plugin, marketplace, scope} = {}) {
  if ((plugin && (result.pluginId ?? result.plugin) !== plugin)
      || (marketplace && result.marketplace !== marketplace)
      || (scope && result.scope !== scope)) {
    throw new InstallerError('Claude Code returned an unexpected plugin, marketplace, or scope. Review its native plugin state before retrying.', 'UNEXPECTED_IDENTITY');
  }
  return result;
}

const MINIMUM_CLAUDE = [2, 1, 289];

function requireSupportedVersion(stdout) {
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*) \(Claude Code\)$/.exec(stdout.trim());
  const version = match?.slice(1).map(Number);
  if (!version || !version.every(Number.isSafeInteger)) {
    throw new InstallerError('The selected executable did not identify itself as Claude Code. Use the Claude Code CLI on PATH or select it with --claude PATH.', 'INVALID_CLAUDE_EXECUTABLE');
  }
  const different = version.findIndex((part, index) => part !== MINIMUM_CLAUDE[index]);
  if (different >= 0 && version[different] < MINIMUM_CLAUDE[different]) {
    throw new InstallerError(`DeLM supports Claude Code ${MINIMUM_CLAUDE.join('.')} or newer; found ${version.join('.')}. Update Claude Code and retry.`, 'UNSUPPORTED_CLAUDE_VERSION');
  }
}

export async function manageClaude(command, {claude, release, run}) {
  const invoke = async args => {
    try {
      return await run(claude, args);
    } catch (error) {
      if (error.code === 'MISSING_EXECUTABLE') {
        throw new InstallerError('Install Claude Code CLI and make `claude` available on PATH, or pass --claude PATH.', error.code);
      }
      throw error;
    }
  };
  // This is our qualified support floor, not a claim about the API's first release.
  // Keep status/removal available when older native plugin JSON remains compatible.
  if (command === 'install' || command === 'update') {
    requireSupportedVersion((await invoke(['--version'])).stdout);
  }
  const native = async (args, mutation = null) => {
    let response;
    let failure;
    try {
      response = await invoke(['plugin', ...args, '--json']);
    } catch (error) {
      if (!mutation || !error.stdout?.trim()) throw error;
      response = {stdout: error.stdout};
      failure = error;
    }
    let result;
    try {
      result = parse(response.stdout, Boolean(mutation));
    } catch (error) {
      throw failure ?? error;
    }
    if (mutation) {
      if (!result || result.command !== mutation.command || !['ok', 'failed'].includes(result.outcome)) throw unsupported();
      // A racing enable can report its already-satisfied goal with exit 1.
      // Accept only that precise native response; state is read again below.
      const alreadyEnabled = mutation.command === 'enable' && result.outcome === 'failed'
        && result.failureCode === 'already_in_goal_state' && result.alreadyInGoalState === true;
      if ((failure || result.outcome !== 'ok') && !alreadyEnabled) {
        throw failure ?? new InstallerError(`Claude Code ${mutation.command} failed: ${result.message ?? result.failureCode ?? 'unknown native error'}`, 'NATIVE_COMMAND_FAILED');
      }
      checkIdentity(result, mutation);
    }
    return result;
  };
  const readState = async () => {
    const marketplaces = await native(['marketplace', 'list']);
    const installed = await native(['list']);
    if (!Array.isArray(marketplaces) || !Array.isArray(installed)
        || marketplaces.some(item => !item || typeof item.name !== 'string')
        || installed.some(item => !item || typeof item.id !== 'string' || typeof item.scope !== 'string' || typeof item.enabled !== 'boolean')) throw unsupported();
    const matching = marketplaces.filter(item => item.name === release.marketplace);
    const marketplace = matching[0];
    const expectedSource = marketplace?.ref === release.ref && (
      (marketplace.source === 'git' && [release.url, release.url.slice(0, -4)].includes(marketplace.url))
      || (marketplace.source === 'github' && marketplace.repo === release.repository));
    const plugins = installed.filter(item => item.id === release.plugin);
    const userPlugins = plugins.filter(item => item.scope === 'user');
    const plugin = userPlugins[0];
    return {
      plugin: release.plugin, scope: 'user',
      marketplaceRegistered: Boolean(marketplace),
      installed: Boolean(plugin), enabled: Boolean(plugin?.enabled), version: plugin?.version ?? null,
      legacyInstalled: installed.some(item => item.id === 'delm@delm-local'),
      scopeConflict: plugins.length !== userPlugins.length || userPlugins.length > 1,
      conflict: matching.length > 1 || Boolean(marketplace && !expectedSource),
    };
  };
  const before = await readState();
  if (command === 'status') return {command, ...before};
  if (before.conflict || (before.installed && !before.marketplaceRegistered)) {
    throw new InstallerError('The delm marketplace source or branch does not match this installer, or its registration is missing. It was preserved. Review `claude plugin marketplace list --json` before continuing.', 'MARKETPLACE_CONFLICT');
  }
  if (before.scopeConflict) {
    throw new InstallerError('Claude Code reports DeLM in another scope or more than once. This installer manages only user scope. Resolve the registrations with Claude Code before continuing; nothing was changed.', 'SCOPE_CONFLICT');
  }
  if (before.legacyInstalled && command !== 'remove') {
    throw new InstallerError('An existing delm@delm-local Claude plugin was preserved. Stop active DeLM work and review that installation in Claude Code before installing the public plugin.', 'LEGACY_INSTALLATION');
  }
  const mutation = (operation, extra = {}) => ({command: operation, plugin: release.plugin, scope: 'user', ...extra});
  if (command === 'remove') {
    if (before.installed) await native(['uninstall', release.plugin, '--scope', 'user', '--keep-data'], mutation('uninstall'));
    const after = await readState();
    if (after.installed || after.conflict || after.scopeConflict) throw new InstallerError('The expected DeLM removal could not be verified. Review `claude plugin list --json`.', 'VERIFICATION_FAILED');
    return {command, changed: before.installed, ...after};
  }
  if (command === 'update' && !before.installed) throw new InstallerError('DeLM is not installed in Claude Code. Run `npx --yes delm-agent@latest install --host claude` first.', 'NOT_INSTALLED');
  let changed = !before.installed || !before.enabled;
  if (!before.marketplaceRegistered || !before.installed || command === 'update') await run('git', ['--version']);
  if (!before.marketplaceRegistered) {
    await native(['marketplace', 'add', `${release.url}#${release.ref}`, '--scope', 'user'], {command: 'marketplace-add', marketplace: release.marketplace});
    // Confirm the source/ref before installing code from a newly registered catalog.
    const registered = await readState();
    if (!registered.marketplaceRegistered || registered.conflict || registered.scopeConflict) throw new InstallerError('The expected marketplace registration could not be verified. No plugin was installed.', 'VERIFICATION_FAILED');
  }
  if (command === 'update') {
    await native(['marketplace', 'update', release.marketplace], {command: 'marketplace-update', marketplace: release.marketplace});
    // Catalog refresh may change identity or source. Re-read before updating.
    const refreshed = await readState();
    if (refreshed.conflict || refreshed.scopeConflict || !refreshed.marketplaceRegistered) throw new InstallerError('The marketplace changed during refresh. No plugin update was requested.', 'VERIFICATION_FAILED');
    await native(['update', release.plugin, '--scope', 'user'], mutation('update'));
    changed = true;
  } else if (!before.installed) {
    await native(['install', release.plugin, '--scope', 'user'], mutation('install'));
  }
  let after = await readState();
  if (command === 'install' && after.installed && !after.enabled && !after.conflict && !after.scopeConflict) {
    await native(['enable', release.plugin, '--scope', 'user'], mutation('enable'));
    after = await readState();
  }
  if (!after.marketplaceRegistered || !after.installed || after.conflict || after.scopeConflict || after.legacyInstalled
      || (command === 'install' && !after.enabled) || (command === 'update' && before.enabled !== after.enabled)) {
    throw new InstallerError('The expected DeLM installation could not be verified. Review `claude plugin list --json` before retrying.', 'VERIFICATION_FAILED');
  }
  return {command, changed, ...after};
}

export function describeClaude(result) {
  const label = result.version ? `DeLM ${result.version}` : 'DeLM';
  if (result.command === 'status') {
    const lines = [result.installed ? `${label} (delm@delm) is ${result.enabled ? 'enabled' : 'disabled'} in Claude Code (user scope).` : 'DeLM is not installed in Claude Code at user scope.'];
    if (result.conflict) lines.push('The delm marketplace source or branch differs. Review `claude plugin marketplace list --json`.');
    if (result.scopeConflict) lines.push('DeLM is registered in another scope or more than once. Review `claude plugin list --json`.');
    if (result.legacyInstalled) lines.push('A source installation (delm@delm-local) is also present.');
    return lines.join('\n');
  }
  if (result.command === 'remove') return result.changed
    ? 'Removed DeLM (delm@delm) from Claude Code user scope. Saved plugin data and the marketplace registration were retained.'
    : 'DeLM is not installed in Claude Code at user scope. Nothing changed.';
  if (result.command === 'update' && !result.enabled) return `Updated ${label} through Claude Code.\nDeLM remains disabled. Use install --host claude to enable it.`;
  return [result.command === 'update' ? `Updated ${label} through Claude Code.`
    : result.changed ? `Installed ${label} in Claude Code.` : `${label} is already installed and enabled in Claude Code. No reinstall was needed.`,
  'Restart Claude Code to load the plugin, then use /delm:run <task>.',
  'Installation does not start workers or grant tool permissions.'].join('\n');
}
