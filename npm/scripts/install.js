#!/usr/bin/env node

const https = require('https');
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

const VERSION = require('../package.json').version;
const REPO = 'so-keyldzn/semantiq';

function getPlatform() {
  const platform = process.platform;
  const arch = process.arch;

  const platforms = {
    'darwin-x64': 'x86_64-apple-darwin',
    'darwin-arm64': 'aarch64-apple-darwin',
    'linux-x64': 'x86_64-unknown-linux-gnu',
    'linux-arm64': 'aarch64-unknown-linux-gnu',
    'win32-x64': 'x86_64-pc-windows-msvc',
  };

  const key = `${platform}-${arch}`;
  const target = platforms[key];

  if (!target) {
    console.error(`Unsupported platform: ${key}`);
    console.error('Supported platforms:', Object.keys(platforms).join(', '));
    process.exit(1);
  }

  return { target, isWindows: platform === 'win32' };
}

function downloadFile(url, dest) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);

    const request = (url) => {
      https.get(url, (response) => {
        if (response.statusCode === 302 || response.statusCode === 301) {
          request(response.headers.location);
          return;
        }

        if (response.statusCode !== 200) {
          reject(new Error(`Failed to download: ${response.statusCode}`));
          return;
        }

        response.pipe(file);
        file.on('finish', () => {
          file.close();
          resolve();
        });
      }).on('error', reject);
    };

    request(url);
  });
}

async function install() {
  const { target, isWindows } = getPlatform();
  const binName = isWindows ? 'semantiq.exe' : 'semantiq';
  const archiveName = `semantiq-v${VERSION}-${target}.tar.gz`;
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${archiveName}`;

  const binDir = path.join(__dirname, '..', 'bin');
  const binPath = path.join(binDir, binName);
  const archivePath = path.join(binDir, archiveName);

  console.log(`Downloading Semantiq v${VERSION} for ${target}...`);

  try {
    await downloadFile(url, archivePath);

    // Extract
    if (isWindows) {
      execSync(`tar -xzf "${archivePath}" -C "${binDir}"`, { stdio: 'inherit' });
    } else {
      execSync(`tar -xzf "${archivePath}" -C "${binDir}"`, { stdio: 'inherit' });
      fs.chmodSync(binPath, 0o755);
    }

    // Cleanup
    fs.unlinkSync(archivePath);

    console.log('Semantiq installed successfully!');
  } catch (error) {
    console.error('Failed to install Semantiq:', error.message);
    console.error('');
    console.error('Alternative installation methods:');
    console.error('  brew install so-keyldzn/tap/semantiq');
    console.error('  cargo install semantiq');
    process.exit(1);
  }
}

install();
