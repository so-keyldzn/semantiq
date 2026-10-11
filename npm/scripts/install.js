#!/usr/bin/env node

// Downloads the semantiq release binary for this platform at `npm install`.
// Keep this file compatible with the Node version `engines` declares in
// package.json (no fetch, no AbortSignal.timeout). `node --test` covers the
// download helpers against a local HTTP server: `npm test` in npm/.

const https = require('https');
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { execSync } = require('child_process');

const VERSION = require('../package.json').version;
const REPO = 'so-keyldzn/semantiq';

// GitHub answers release-asset downloads with a chain of redirects (302 to
// objects.githubusercontent.com, 307/308 on some CDN paths).
const MAX_REDIRECTS = 5;
const REDIRECT_STATUSES = [301, 302, 303, 307, 308];
// A socket idle for this long aborts the request: a stalled CDN connection
// must not hang `postinstall` forever.
const REQUEST_TIMEOUT_MS = 30000;
// Transient failures (network errors, timeouts, 429/5xx) are retried with
// exponential backoff: 1 s, then 2 s, before giving up.
const RETRY_ATTEMPTS = 3;
const RETRY_BASE_DELAY_MS = 1000;

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

// A final non-200 status. A missing asset (404) stays missing: not retried.
class HttpStatusError extends Error {
  constructor(statusCode, url) {
    super(`Failed to download ${url}: HTTP ${statusCode}`);
    this.name = 'HttpStatusError';
    this.statusCode = statusCode;
    this.retryable = statusCode === 429 || statusCode >= 500;
  }
}

// GET `url` and resolve with the 200 response (a readable stream), following
// up to MAX_REDIRECTS redirects. Every redirect response is drained so its
// socket is released. `options.get` replaces `https.get` (tests use `http`)
// and `options.timeoutMs` the idle timeout.
function getFollowingRedirects(url, options) {
  const opts = options || {};
  const get = opts.get || https.get;
  const timeoutMs = opts.timeoutMs || REQUEST_TIMEOUT_MS;
  const hops = opts.hops || 0;

  return new Promise((resolve, reject) => {
    let finalResponse = null;
    const request = get(url, (response) => {
      const status = response.statusCode;

      if (REDIRECT_STATUSES.includes(status)) {
        response.resume();
        const location = response.headers.location;
        if (!location) {
          reject(new Error(`Redirect (HTTP ${status}) without a Location header from ${url}`));
          return;
        }
        if (hops >= MAX_REDIRECTS) {
          reject(new Error(`Too many redirects (more than ${MAX_REDIRECTS}) downloading ${url}`));
          return;
        }
        const next = new URL(location, url).toString();
        resolve(getFollowingRedirects(next, { get, timeoutMs, hops: hops + 1 }));
        return;
      }

      if (status !== 200) {
        response.resume();
        reject(new HttpStatusError(status, url));
        return;
      }

      finalResponse = response;
      resolve(response);
    });

    request.setTimeout(timeoutMs, () => {
      const err = new Error(`Timed out after ${timeoutMs / 1000}s waiting for ${url}`);
      // A response already streaming only reports "aborted": keep the cause.
      if (finalResponse) finalResponse.timeoutError = err;
      request.destroy(err);
    });
    request.on('error', reject);
  });
}

// Download `url` into `dest`. A partial file is removed on failure.
function downloadFile(url, dest, options) {
  return getFollowingRedirects(url, options).then(
    (response) =>
      new Promise((resolve, reject) => {
        const file = fs.createWriteStream(dest);
        let settled = false;
        const fail = (err) => {
          if (settled) return;
          settled = true;
          response.destroy();
          file.destroy();
          fs.unlink(dest, () => reject(err));
        };

        response.on('error', (err) => fail(response.timeoutError || err));
        file.on('error', fail);
        file.on('finish', () => {
          file.close((err) => {
            if (err) {
              fail(err);
              return;
            }
            settled = true;
            resolve();
          });
        });
        response.pipe(file);
      })
  );
}

// Download a small text resource (the .sha256 file) into memory.
function downloadText(url, options) {
  return getFollowingRedirects(url, options).then(
    (response) =>
      new Promise((resolve, reject) => {
        let data = '';
        response.setEncoding('utf8');
        response.on('data', (chunk) => {
          data += chunk;
        });
        response.on('end', () => resolve(data));
        response.on('error', (err) => reject(response.timeoutError || err));
      })
  );
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// Run `attempt` up to `attempts` times, waiting baseDelayMs * 2^(n-1) between
// tries. An error flagged `retryable: false` (a 404) is thrown at once.
async function withRetry(label, attempt, options) {
  const opts = options || {};
  const attempts = opts.attempts || RETRY_ATTEMPTS;
  const baseDelayMs = opts.baseDelayMs === undefined ? RETRY_BASE_DELAY_MS : opts.baseDelayMs;
  const log = opts.log || ((message) => console.error(message));

  for (let n = 1; ; n++) {
    try {
      return await attempt();
    } catch (err) {
      if (n >= attempts || err.retryable === false) {
        throw err;
      }
      const delay = baseDelayMs * 2 ** (n - 1);
      log(`${label} failed (${err.message}); retrying in ${delay / 1000}s (attempt ${n + 1}/${attempts})...`);
      await sleep(delay);
    }
  }
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
    await withRetry('Download', () => downloadFile(url, archivePath));

    // Integrity verification: never extract/execute an unverified binary.
    // The release CI publishes "<archive>.sha256" next to each artifact.
    let expectedSha;
    try {
      const checksumRaw = await withRetry('Checksum download', () => downloadText(checksumUrl));
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
    console.log('Next: run `semantiq init` in your project (installs the Claude Code skill + MCP server and indexes it),');
    console.log('or `semantiq init --global` to install the skill for all your projects.');
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

if (require.main === module) {
  install();
} else {
  module.exports = {
    MAX_REDIRECTS,
    HttpStatusError,
    getFollowingRedirects,
    downloadFile,
    downloadText,
    withRetry,
    parseExpectedSha256,
  };
}
