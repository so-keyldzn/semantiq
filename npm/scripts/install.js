#!/usr/bin/env node

const https = require('https');
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
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

    const cleanup = () => {
      file.close();
      fs.unlink(dest, () => {});
    };

    const request = (url) => {
      https.get(url, (response) => {
        if (response.statusCode === 302 || response.statusCode === 301) {
          request(response.headers.location);
          return;
        }

        if (response.statusCode !== 200) {
          cleanup();
          reject(new Error(`Failed to download: ${response.statusCode}`));
          return;
        }

        response.pipe(file);
        file.on('finish', () => {
          file.close();
          resolve();
        });
        response.on('error', (err) => {
          cleanup();
          reject(err);
        });
      }).on('error', (err) => {
        cleanup();
        reject(err);
      });
    };

    request(url);
  });
}

// Download a small text resource (the .sha256 file) directly into memory,
// following redirects. Rejects on any non-200 final status.
function downloadText(url) {
  return new Promise((resolve, reject) => {
    const request = (url) => {
      https.get(url, (response) => {
        if (response.statusCode === 302 || response.statusCode === 301) {
          request(response.headers.location);
          return;
        }

        if (response.statusCode !== 200) {
          reject(new Error(`Failed to download checksum: ${response.statusCode}`));
          return;
        }

        let data = '';
        response.setEncoding('utf8');
        response.on('data', (chunk) => {
          data += chunk;
        });
        response.on('end', () => resolve(data));
        response.on('error', reject);
      }).on('error', reject);
    };

    request(url);
  });
}

function sha256File(filePath) {
  const hash = crypto.createHash('sha256');
  hash.update(fs.readFileSync(filePath));
  return hash.digest('hex');
}

// The published .sha256 may be either a bare hex digest or the standard
// `sha256sum` format: "<hex>  <filename>". Extract the leading hex token.
function parseExpectedSha256(raw, archiveName) {
  const tokens = raw.trim().split(/\s+/);
  const hex = (tokens[0] || '').toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(hex)) {
    throw new Error(`Malformed checksum file for ${archiveName}`);
  }
  return hex;
}

async function install() {
  const { target, isWindows } = getPlatform();
  const binName = isWindows ? 'semantiq.exe' : 'semantiq';
  const archiveName = `semantiq-v${VERSION}-${target}.tar.gz`;
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${archiveName}`;
  const checksumUrl = `${url}.sha256`;

  const binDir = path.join(__dirname, '..', 'bin');
  const binPath = path.join(binDir, binName);
  const archivePath = path.join(binDir, archiveName);

  // Best-effort removal of partial/untrusted artifacts.
  const removeArtifacts = () => {
    for (const p of [archivePath, binPath]) {
      try {
        if (fs.existsSync(p)) fs.unlinkSync(p);
      } catch (_) {
        // ignore
      }
    }
  };

  console.log(`Downloading Semantiq v${VERSION} for ${target}...`);

  try {
    await downloadFile(url, archivePath);

    // Integrity verification: never extract/execute an unverified binary.
    // The release CI publishes "<archive>.sha256" next to each artifact.
    let expectedSha;
    try {
      const checksumRaw = await downloadText(checksumUrl);
      expectedSha = parseExpectedSha256(checksumRaw, archiveName);
    } catch (err) {
      removeArtifacts();
      throw new Error(
        `Could not verify integrity (missing or unreadable ${archiveName}.sha256): ${err.message}`
      );
    }

    const actualSha = sha256File(archivePath);
    if (actualSha !== expectedSha) {
      removeArtifacts();
      throw new Error(
        `Checksum mismatch for ${archiveName}\n  expected: ${expectedSha}\n  actual:   ${actualSha}`
      );
    }

    console.log('Checksum verified.');

    // Extract (verified archive only)
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
    removeArtifacts();
    console.error('Failed to install Semantiq:', error.message);
    console.error('');
    console.error('Alternative installation methods:');
    console.error('  brew install so-keyldzn/tap/semantiq');
    console.error('  cargo install semantiq');
    process.exit(1);
  }
}

install();
