#!/usr/bin/env node

// Cross-platform launcher for the native binary downloaded by scripts/install.js.
// npm generates the .cmd/.ps1 shims on Windows from this file's shebang, so it
// must be a Node script: a /bin/sh entry point breaks the Windows shims (#16).

const { spawnSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const binName = process.platform === 'win32' ? 'semantiq.exe' : 'semantiq';
const binPath = path.join(__dirname, binName);

if (!fs.existsSync(binPath)) {
  console.error(`Semantiq binary not found: ${binPath}`);
  console.error("Please run 'npm install' again.");
  process.exit(1);
}

const result = spawnSync(binPath, process.argv.slice(2), { stdio: 'inherit' });

if (result.error) {
  console.error(`Failed to run Semantiq: ${result.error.message}`);
  process.exit(1);
}

if (result.signal) {
  process.kill(process.pid, result.signal);
}

process.exit(result.status === null ? 1 : result.status);
