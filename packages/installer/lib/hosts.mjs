import {constants} from 'node:fs';
import {access, stat} from 'node:fs/promises';
import {delimiter, resolve} from 'node:path';
import {createInterface} from 'node:readline';
import {InstallerError, manage, validateRequest} from './installer.mjs';

export const HOST_LABELS = {codex: 'Codex', claude: 'Claude Code'};
const HOSTS = Object.keys(HOST_LABELS);

export function validateHostOptions({host, codex, claude} = {}) {
  if (host !== undefined && ![...HOSTS, 'both'].includes(host)) {
    throw new InstallerError('Choose --host codex, --host claude, or --host both.', 'USAGE');
  }
  for (const [name, executable] of Object.entries({codex, claude})) {
    if (executable !== undefined && (typeof executable !== 'string' || !executable.trim())) {
      throw new InstallerError(`--${name} requires an executable path.`, 'USAGE');
    }
    if (executable !== undefined && host !== undefined && host !== 'both' && host !== name) {
      throw new InstallerError(`--${name} requires --host ${name} or --host both.`, 'USAGE');
    }
  }
  if (host === 'both') return [...HOSTS];
  if (host !== undefined) return [host];
  const explicit = HOSTS.filter(name => ({codex, claude})[name] !== undefined);
  return explicit.length ? explicit : null;
}

// Discovery inspects PATH entries without starting a host or loading its settings.
export async function discoverHosts({env = process.env, platform = process.platform, cwd = process.cwd()} = {}) {
  const available = {};
  const directories = (env.PATH ?? '').split(platform === 'win32' ? ';' : delimiter);
  const extensions = platform === 'win32' ? (env.PATHEXT ?? '.COM;.EXE;.BAT;.CMD').split(';') : [''];
  if (env.PATH === undefined) return available;
  for (const host of HOSTS) {
    for (const directory of directories) {
      for (const extension of extensions) {
        const executable = resolve(cwd, directory, `${host}${extension}`);
        try {
          if (!(await stat(executable)).isFile()) continue;
          await access(executable, platform === 'win32' ? constants.F_OK : constants.X_OK);
          available[host] = executable;
          break;
        } catch (error) {
          if (!['ENOENT', 'ENOTDIR', 'EACCES', 'EPERM', 'ELOOP'].includes(error.code)) throw error;
        }
      }
      if (available[host]) break;
    }
  }
  return available;
}

export function promptForHosts({input = process.stdin, output = process.stderr, signals = process} = {}) {
  return new Promise((resolveSelection, reject) => {
    const terminal = createInterface({input, output, terminal: Boolean(input.isTTY && output.isTTY)});
    let settled = false;
    const finish = (selection, error) => {
      if (settled) return;
      settled = true;
      terminal.removeListener('line', onLine);
      terminal.removeListener('close', cancel);
      terminal.removeListener('SIGINT', cancel);
      signals.removeListener('SIGINT', cancel);
      signals.removeListener('SIGTERM', cancel);
      terminal.close();
      if (error) reject(error);
      else resolveSelection(selection);
    };
    const cancel = () => finish(null, new InstallerError('Cancelled. No plugins were changed.', 'CANCELLED'));
    const onLine = answer => {
      const choice = {'1': ['codex'], '2': ['claude'], '3': [...HOSTS]}[answer.trim()];
      if (choice) finish(choice);
      else {
        output.write('Enter 1, 2, or 3. Press Ctrl-C to cancel.\n');
        terminal.prompt();
      }
    };
    terminal.on('line', onLine);
    terminal.once('close', cancel);
    terminal.once('SIGINT', cancel);
    signals.once('SIGINT', cancel);
    signals.once('SIGTERM', cancel);
    output.write('Choose where to manage DeLM:\n\n  1. Codex\n  2. Claude Code\n  3. Both\n\n');
    terminal.setPrompt('Choose 1, 2, or 3: ');
    terminal.prompt();
  });
}

export async function selectHosts(options = {}, {
  interactive = Boolean(process.stdin.isTTY && process.stdout.isTTY && process.stderr.isTTY),
  discover = discoverHosts,
  prompt = promptForHosts,
} = {}) {
  let selected = validateHostOptions(options);
  let available = {};
  if (!selected) {
    available = await discover();
    const detected = HOSTS.filter(host => available[host]);
    if (!detected.length) {
      throw new InstallerError('No Codex or Claude Code CLI was found on PATH. Install either CLI first, or pass --codex PATH or --claude PATH. A desktop or IDE installation alone is insufficient.', 'MISSING_HOST');
    }
    if (detected.length === 1) selected = detected;
    else if (options.json || !interactive) {
      throw new InstallerError('Both Codex and Claude Code are available. Choose --host codex, --host claude, or --host both. Host selection requires an interactive terminal without --json.', 'HOST_SELECTION_REQUIRED');
    } else selected = await prompt();
  }
  return selected.map(host => ({host, [host]: options[host] ?? available[host] ?? host}));
}

export async function manageHosts(command, options = {}, {
  validate = validateRequest,
  select = selectHosts,
  run = manage,
  selection,
} = {}) {
  validateHostOptions(options);
  validate(command);
  const selected = await select(options, selection);
  if (selected.length === 1) return run(command, selected[0]);
  const results = [];
  const errors = [];
  for (const hostOptions of selected) {
    try {
      results.push(await run(command, hostOptions));
    } catch (error) {
      errors.push({host: hostOptions.host, error: error.message, code: error.code ?? 'INSTALLER_ERROR'});
    }
  }
  return {command, results, errors, success: errors.length === 0};
}
