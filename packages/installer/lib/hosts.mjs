import {constants} from 'node:fs';
import {access, stat} from 'node:fs/promises';
import {delimiter, resolve} from 'node:path';
import {createInterface} from 'node:readline';
import {InstallerError, manage, validateRequest} from './installer.mjs';

export const HOST_LABELS = {codex: 'Codex', claude: 'Claude Code', pi: 'Pi'};
const HOSTS = Object.keys(HOST_LABELS);
// `both` predates the Pi host and keeps its meaning; `all` covers every host.
const GROUPS = {both: ['codex', 'claude'], all: [...HOSTS]};
const CHOICES = 'Choose --host codex, --host claude, --host pi, --host both, or --host all.';

export function validateHostOptions({host, codex, claude, pi} = {}) {
  const executables = {codex, claude, pi};
  if (host !== undefined && ![...HOSTS, ...Object.keys(GROUPS)].includes(host)) {
    throw new InstallerError(CHOICES, 'USAGE');
  }
  for (const [name, executable] of Object.entries(executables)) {
    if (executable !== undefined && (typeof executable !== 'string' || !executable.trim())) {
      throw new InstallerError(`--${name} requires an executable path.`, 'USAGE');
    }
    if (executable !== undefined && host !== undefined && !(GROUPS[host] ?? [host]).includes(name)) {
      throw new InstallerError(`--${name} requires --host ${name}, or a host group that includes it.`, 'USAGE');
    }
  }
  if (host !== undefined) return [...(GROUPS[host] ?? [host])];
  const explicit = HOSTS.filter(name => executables[name] !== undefined);
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

export function promptForHosts({input = process.stdin, output = process.stderr, signals = process, hosts = HOSTS} = {}) {
  const offered = HOSTS.filter(host => hosts.includes(host));
  const everything = offered.length > 2 ? 'All' : 'Both';
  const choices = Object.fromEntries([...offered.map((host, index) => [String(index + 1), [host]]), [String(offered.length + 1), [...offered]]]);
  const range = offered.length > 1 ? offered.map((_, index) => index + 1).join(', ') + `, or ${offered.length + 1}` : '1 or 2';
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
      const choice = choices[answer.trim()];
      if (choice) finish(choice);
      else {
        output.write(`Enter ${range}. Press Ctrl-C to cancel.\n`);
        terminal.prompt();
      }
    };
    terminal.on('line', onLine);
    terminal.once('close', cancel);
    terminal.once('SIGINT', cancel);
    signals.once('SIGINT', cancel);
    signals.once('SIGTERM', cancel);
    const menu = [...offered.map((host, index) => `  ${index + 1}. ${HOST_LABELS[host]}`), `  ${offered.length + 1}. ${everything}`];
    output.write('Choose where to manage DeLM:\n\n' + menu.join('\n') + '\n\n');
    terminal.setPrompt(`Choose ${range}: `);
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
      throw new InstallerError('No Codex, Claude Code or Pi CLI was found on PATH. Install one first, or pass --codex PATH, --claude PATH or --pi PATH. A desktop or IDE installation alone is insufficient.', 'MISSING_HOST');
    }
    if (detected.length === 1) selected = detected;
    else if (options.json || !interactive) {
      throw new InstallerError(`${detected.map(host => HOST_LABELS[host]).join(' and ')} are available. ${CHOICES} Host selection requires an interactive terminal without --json.`, 'HOST_SELECTION_REQUIRED');
    } else selected = await prompt({hosts: detected});
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
