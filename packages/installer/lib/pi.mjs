import {InstallerError} from './native.mjs';

// Pi installs a git source as a package. The release's marketplace branch carries the
// prebuilt Pi package and a root manifest that points at it, so one source string
// identifies the installation: `git:github.com/OWNER/REPO@marketplace`.
export function piSource(release, repository = release.repository) {
  return `git:github.com/${repository}@${release.ref}`;
}

const MINIMUM_PI = [1, 4, 0];

function requireSupportedVersion(stdout) {
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)/.exec(stdout.trim());
  const version = match?.slice(1).map(Number);
  if (!version || !version.every(Number.isSafeInteger)) {
    throw new InstallerError('The selected executable did not identify itself as Pi. Use the Pi CLI on PATH or select it with --pi PATH.', 'INVALID_PI_EXECUTABLE');
  }
  const different = version.findIndex((part, index) => part !== MINIMUM_PI[index]);
  if (different >= 0 && version[different] < MINIMUM_PI[different]) {
    throw new InstallerError(`DeLM supports Pi ${MINIMUM_PI.join('.')} or newer; found ${version.join('.')}. Update Pi and retry.`, 'UNSUPPORTED_PI_VERSION');
  }
}

// `pi list` prints sections ("User packages:", "Project packages:"); each package is a
// two-space-indented source line followed by its resolved location, indented further.
export function parsePackageList(stdout) {
  const sections = {user: [], project: []};
  let current = null;
  for (const raw of String(stdout).split(/\r?\n/)) {
    const line = raw.replace(/\s+$/, '');
    if (!line.trim()) continue;
    if (/^user packages:/i.test(line)) { current = 'user'; continue; }
    if (/^project packages:/i.test(line)) { current = 'project'; continue; }
    if (/^[^\s]/.test(line)) { current = null; continue; }
    if (!current) continue;
    const indent = line.length - line.trimStart().length;
    if (indent <= 2) sections[current].push(line.trim());
  }
  return sections;
}

function repositoryVariants(release) {
  return [release.repository, ...(release.previousRepositories ?? [])].map(repository => repository.toLowerCase());
}

// A source for this repository in any other spelling (another ref, an https URL, no ref)
// is a registration this installer did not make; it is reported, never replaced.
function refersToRelease(source, release) {
  const text = source.toLowerCase();
  return repositoryVariants(release).some(repository => text.includes(`github.com/${repository}`) || text.includes(`github.com:${repository}`));
}

export async function managePi(command, {pi, release, run, beforeMutation}) {
  const invoke = async args => {
    try {
      return await run(pi, args);
    } catch (error) {
      if (error.code === 'MISSING_EXECUTABLE') {
        throw new InstallerError('Install Pi (`npm install -g @earendil-works/pi-coding-agent`) and make `pi` available on PATH, or pass --pi PATH.', error.code);
      }
      throw error;
    }
  };
  if (command === 'install' || command === 'update') {
    requireSupportedVersion((await invoke(['--version'])).stdout);
  }
  const expected = [release.repository, ...(release.previousRepositories ?? [])].map(repository => piSource(release, repository));
  const readState = async () => {
    const listed = parsePackageList((await invoke(['list'])).stdout);
    const installedSources = listed.user.filter(source => expected.includes(source));
    const variants = listed.user.filter(source => !expected.includes(source) && refersToRelease(source, release));
    const project = listed.project.filter(source => refersToRelease(source, release));
    return {
      plugin: 'delm-pi', source: installedSources[0] ?? piSource(release), scope: 'user',
      installed: installedSources.length > 0, enabled: installedSources.length > 0, version: null,
      legacyInstalled: false,
      conflict: variants.length > 0 || installedSources.length > 1,
      scopeConflict: project.length > 0,
    };
  };
  const before = await readState();
  if (command === 'status') return {command, ...before};
  if (before.conflict) {
    throw new InstallerError('Pi lists this repository under another source or reference. It was preserved. Review `pi list` and remove the other registration before continuing.', 'MARKETPLACE_CONFLICT');
  }
  if (before.scopeConflict) {
    throw new InstallerError('Pi lists DeLM as a project package. This installer manages only your user packages. Resolve the project registration with Pi before continuing; nothing was changed.', 'SCOPE_CONFLICT');
  }
  if (command === 'remove') {
    if (before.installed) {
      await beforeMutation();
      await invoke(['remove', before.source]);
    }
    const after = await readState();
    if (after.installed || after.conflict) throw new InstallerError('The expected DeLM removal could not be verified. Review `pi list`.', 'VERIFICATION_FAILED');
    return {command, changed: before.installed, ...after};
  }
  if (command === 'update' && !before.installed) {
    throw new InstallerError('DeLM is not installed in Pi. Run `npx --yes delm-agent@latest install --host pi` first.', 'NOT_INSTALLED');
  }
  let changed = !before.installed;
  await run('git', ['--version']);
  if (command === 'update') {
    await beforeMutation();
    await invoke(['update', '--extension', before.source]);
    changed = true;
  } else if (!before.installed) {
    await beforeMutation();
    await invoke(['install', piSource(release)]);
  }
  const after = await readState();
  if (!after.installed || after.conflict || after.scopeConflict) {
    throw new InstallerError('The expected DeLM installation could not be verified. Review `pi list` before retrying.', 'VERIFICATION_FAILED');
  }
  return {command, changed, ...after};
}

export function describePi(result) {
  if (result.command === 'status') {
    const lines = [result.installed ? `DeLM (delm-pi) is installed in Pi from ${result.source}.` : 'DeLM is not installed in Pi.'];
    if (result.conflict) lines.push('Pi lists this repository under another source or reference. Review `pi list`.');
    if (result.scopeConflict) lines.push('DeLM is also registered as a project package. Review `pi list`.');
    if (result.installed) {
      lines.push('Session readiness has not been checked.');
      lines.push(...(result.readiness?.nextSteps ?? []));
    }
    return lines.join('\n');
  }
  if (result.command === 'remove') return result.changed
    ? 'Removed DeLM (delm-pi) from Pi. Saved DeLM runs were retained.'
    : 'DeLM is not installed in Pi. Nothing changed.';
  return [result.command === 'update' ? 'Updated DeLM through Pi.'
    : result.changed ? 'Installed DeLM in Pi.' : 'DeLM is already installed in Pi. No reinstall was needed.',
  'Start a new Pi session to load the plugin, then use /delm:run <task>.',
  'Installation does not start workers or change Pi\'s settings beyond the package list.'].join('\n');
}
