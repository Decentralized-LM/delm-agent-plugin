#!/usr/bin/env node
import {readFile} from 'node:fs/promises';
import {parseArgs} from 'node:util';
import {COMMANDS, InstallerError, RELEASE, describe, manage} from '../lib/installer.mjs';

const help = `DeLM installer for Codex and Claude Code

Usage: delm-agent <install|update|remove|status> [--host codex|claude] [--json]

  install   Install the native DeLM plugin (macOS)
  update    Update DeLM through the selected native plugin manager
  remove    Remove the plugin and retain saved work and the marketplace
  status    Show the native installation state

  --host HOST   codex (default) or claude; manages only the selected host
  --codex PATH  Use an existing Codex CLI executable with --host codex
  --claude PATH Use an existing Claude Code executable with --host claude
  --json        Print structured output
  --help        Show this help
  --version     Show the installer version

Node.js 22+ and the selected host CLI are required. Install/update require Git.
${RELEASE.repository ? `Marketplace: https://github.com/${RELEASE.repository} (branch ${RELEASE.ref}).`
  : 'This unpublished source installer has no release destination. Use a prepared package.'}`;

let json = process.argv.slice(2).includes('--json');
try {
  const {values, positionals} = parseArgs({
    options: {host: {type: 'string'}, codex: {type: 'string'}, claude: {type: 'string'}, json: {type: 'boolean'}, help: {type: 'boolean', short: 'h'}, version: {type: 'boolean', short: 'v'}},
    allowPositionals: true, strict: true,
  });
  json = Boolean(values.json);
  if (values.help || (!positionals.length && !values.version)) {
    console.log(help);
  } else if (values.version) {
    const metadata = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'));
    console.log(json ? JSON.stringify({installerVersion: metadata.version}) : metadata.version);
  } else {
    if (Number(process.versions.node.split('.')[0]) < 22) throw new InstallerError('Node.js 22 or later is required.', 'UNSUPPORTED_NODE');
    if (positionals.length !== 1 || !COMMANDS.includes(positionals[0])) throw new InstallerError('Choose install, update, remove, or status. Use --help for usage.', 'USAGE');
    const result = await manage(positionals[0], {host: values.host, codex: values.codex, claude: values.claude});
    console.log(json ? JSON.stringify(result) : describe(result));
  }
} catch (error) {
  console.error(json ? JSON.stringify({error: error.message, code: error.code ?? 'INSTALLER_ERROR'}) : `DeLM: ${error.message}`);
  process.exitCode = 1;
}
