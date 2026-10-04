#!/usr/bin/env node
import {readFile} from 'node:fs/promises';
import {parseArgs} from 'node:util';
import {COMMANDS, InstallerError, RELEASE, describe} from '../lib/installer.mjs';
import {HOST_LABELS, manageHosts} from '../lib/hosts.mjs';

const help = `DeLM installer for Codex and Claude Code

Usage: delm-agent <install|update|remove|status> [--host codex|claude|both] [--json]

  install   Install the native DeLM plugin (macOS)
  update    Update DeLM through the selected native plugin manager
  remove    Remove the plugin and retain saved work and the marketplace
  status    Show the native installation state

  --host HOST   codex, claude, or both; skips automatic host selection
  --codex PATH  Use an existing Codex CLI executable; implies Codex
  --claude PATH Use an existing Claude Code executable; implies Claude Code
  --json        Print structured output
  --help        Show this help
  --version     Show the installer version

Node.js 22+ and the selected host CLI are required. Install/update require Git.
With one CLI on PATH, it is selected automatically. With both, choose Codex,
Claude Code, or Both interactively. For scripts or --json, specify --host when
both are available. Providing both executable options selects both hosts.
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
    const result = await manageHosts(positionals[0], {host: values.host, codex: values.codex, claude: values.claude, json});
    if (Array.isArray(result.results)) {
      if (json) console.log(JSON.stringify(result));
      else {
        for (const item of result.results) console.log(`${HOST_LABELS[item.host]}:\n${describe(item)}\n`);
        for (const error of result.errors) console.error(`${HOST_LABELS[error.host]}: ${error.error}`);
        if (result.errors.length) console.error('Review the affected host’s native plugin state before retrying.');
      }
      if (!result.success) process.exitCode = 1;
    } else console.log(json ? JSON.stringify(result) : describe(result));
  }
} catch (error) {
  console.error(json ? JSON.stringify({error: error.message, code: error.code ?? 'INSTALLER_ERROR'}) : `DeLM: ${error.message}`);
  process.exitCode = 1;
}
