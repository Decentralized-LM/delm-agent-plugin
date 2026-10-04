import {execFile} from 'node:child_process';
import {promisify} from 'node:util';

const execFileAsync = promisify(execFile);

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
    const failure = new InstallerError(`${file} ${args.join(' ')} failed: ${detail}`, 'NATIVE_COMMAND_FAILED');
    failure.stdout = String(error.stdout ?? '');
    failure.stderr = String(error.stderr ?? '');
    failure.exitCode = error.code;
    throw failure;
  }
}
