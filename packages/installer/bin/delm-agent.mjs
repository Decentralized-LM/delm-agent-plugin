#!/usr/bin/env node
import {readFile} from 'node:fs/promises';
import {parseArgs} from 'node:util';
import {COMMANDS, InstallerError, RELEASE, describe, manage} from '../lib/installer.mjs';

const help = `DeLM installer for Codex

Usage: delm-agent <install|update|remove|status> [--codex PATH] [--json]

  install   Install the native DeLM plugin (macOS)
  update    Update DeLM through Codex's native marketplace manager
  remove    Remove the plugin and retain saved work and the marketplace
  status    Show the native installation state

  --codex PATH  Use an existing Codex CLI executable
  --json        Print structured output
  --help        Show this help
  --version     Show the installer version

Node.js 22+ and stock Codex CLI are required. Install/update also require Git.
${RELEASE.repository ? `Marketplace: https://github.com/${RELEASE.repository} (branch ${RELEASE.ref}).`
  : 'This unpublished source installer has no release destination. Use a prepared package.'}`;

let json = process.argv.slice(2).includes('--json');
try {
  const {values, positionals} = parseArgs({
    options: {codex: {type: 'string'}, json: {type: 'boolean'}, help: {type: 'boolean', short: 'h'}, version: {type: 'boolean', short: 'v'}},
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
    if (values.codex === '') throw new InstallerError('--codex requires an executable path.', 'USAGE');
    const result = await manage(positionals[0], {codex: values.codex});
    console.log(json ? JSON.stringify(result) : describe(result));
  }
} catch (error) {
  console.error(json ? JSON.stringify({error: error.message, code: error.code ?? 'INSTALLER_ERROR'}) : `DeLM: ${error.message}`);
  process.exitCode = 1;
}
