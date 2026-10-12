#!/usr/bin/env node

// Downloads the semantiq release binary for this platform at `npm install`.
// Keep this file compatible with the Node version `engines` declares in
// package.json (no fetch, no AbortSignal.timeout). `node --test` covers the
// helpers and the installer against a local HTTP server: `npm test` in npm/.

const http = require('http');
const https = require('https');
const tls = require('tls');
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { execFileSync } = require('child_process');

const VERSION = require('../package.json').version;
const REPO = 'so-keyldzn/semantiq';
const RELEASE_BASE_URL = `https://github.com/${REPO}/releases/download/v${VERSION}`;

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

// Release targets by `${process.platform}-${process.arch}`. package.json
// declares `os` and `cpu` separately, so npm lets win32-arm64 through: the
// error below is the only check. Linux binaries link glibc: musl (Alpine) is
// refused here instead of failing at launch.
const PLATFORMS = {
  'darwin-x64': 'x86_64-apple-darwin',
  'darwin-arm64': 'aarch64-apple-darwin',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'linux-arm64': 'aarch64-unknown-linux-gnu',
  'win32-x64': 'x86_64-pc-windows-msvc',
};

// The C library of this Node binary on Linux: `glibc`, `musl` or `null` when
// unknown (`process.report` missing or disabled).
function detectLibc() {
  if (process.platform !== 'linux') return null;
  try {
    const header = process.report.getReport().header;
    return header.glibcVersionRuntime ? 'glibc' : 'musl';
  } catch (_) {
    return null;
  }
}

// The release target for a platform, or an Error listing the served ones.
function getPlatform(platform, arch, libc) {
  const key = `${platform}-${arch}`;
  const target = PLATFORMS[key];
  if (!target) {
    throw permanent(
      `Unsupported platform: ${key}\nSupported platforms: ${Object.keys(PLATFORMS).join(', ')}`
    );
  }
  if (platform === 'linux' && libc === 'musl') {
    throw permanent(
      `Unsupported platform: ${key} with musl (Alpine): the Linux binaries link glibc.\n` +
        'Build from source instead: cargo install --git https://github.com/so-keyldzn/semantiq.git'
    );
  }
  return { target, isWindows: platform === 'win32' };
}

// A failure that another attempt cannot fix: thrown through `withRetry`.
function permanent(message) {
  const err = new Error(message);
  err.retryable = false;
  return err;
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

// The proxy to reach `url` through, read like npm does: npm's own config
// (`https-proxy`, then `proxy`, exposed to lifecycle scripts as
// `npm_config_*`), then the usual environment variables. `null` without a
// proxy or when `no_proxy` / `npm_config_noproxy` lists the host.
function proxyFor(url, env) {
  const vars = env || process.env;
  const host = new URL(url).hostname.toLowerCase();
  const noProxy = vars.npm_config_noproxy || vars.NO_PROXY || vars.no_proxy || '';
  for (const raw of noProxy.split(',')) {
    const entry = raw.trim().toLowerCase().replace(/^\./, '');
    if (!entry) continue;
    if (entry === '*' || host === entry || host.endsWith(`.${entry}`)) return null;
  }
  const names = [
    'npm_config_https_proxy',
    'npm_config_proxy',
    'HTTPS_PROXY',
    'https_proxy',
    'ALL_PROXY',
    'all_proxy',
  ];
  for (const name of names) {
    if (vars[name]) return vars[name];
  }
  return null;
}

// A `createConnection` for an http(s) request to `url` that tunnels through
// `proxyUrl` (HTTP CONNECT, Basic proxy credentials from the URL), wrapping
// the tunnel in TLS for an https target. The request's own idle timeout only
// starts once it has a socket, so the CONNECT has its own.
function tunnelThrough(proxyUrl, url, timeoutMs) {
  const proxy = new URL(proxyUrl);
  const target = new URL(url);
  const secure = target.protocol === 'https:';
  const targetPort = target.port || (secure ? 443 : 80);
  const headers = { Host: `${target.hostname}:${targetPort}` };
  if (proxy.username || proxy.password) {
    const user = decodeURIComponent(proxy.username);
    const pass = decodeURIComponent(proxy.password);
    headers['Proxy-Authorization'] = `Basic ${Buffer.from(`${user}:${pass}`).toString('base64')}`;
  }
  const transport = proxy.protocol === 'https:' ? https : http;

  return (_options, callback) => {
    const connect = transport.request({
      host: proxy.hostname,
      port: proxy.port || (proxy.protocol === 'https:' ? 443 : 80),
      method: 'CONNECT',
      path: `${target.hostname}:${targetPort}`,
      headers,
    });
    connect.setTimeout(timeoutMs, () => {
      connect.destroy(new Error(`Timed out after ${timeoutMs / 1000}s connecting to proxy ${proxy.host}`));
    });
    connect.on('error', (err) => callback(new Error(`Proxy ${proxy.host}: ${err.message}`)));
    connect.on('connect', (response, socket) => {
      if (response.statusCode !== 200) {
        socket.destroy();
        callback(permanent(`Proxy ${proxy.host} refused CONNECT to ${target.host}: HTTP ${response.statusCode}`));
        return;
      }
      callback(null, secure ? tls.connect({ socket, servername: target.hostname }) : socket);
    });
    connect.end();
  };
}

// GET `url` and resolve with the 200 response (a readable stream), following
// up to MAX_REDIRECTS redirects. Every redirect response is drained so its
// socket is released. `options.get` replaces `https.get` (tests use `http`),
// `options.timeoutMs` the idle timeout and `options.proxy` the proxy URL
// (`false` disables it). Without `options.proxy`, the proxy is chosen by
// `proxyFor` for every hop, since a redirect changes the host (GitHub sends
// release assets to a CDN) and `no_proxy` may list only one of them;
// `options.env` replaces the environment `proxyFor` reads (tests).
function getFollowingRedirects(url, options) {
  const opts = options || {};
  const get = opts.get || https.get;
  const timeoutMs = opts.timeoutMs || REQUEST_TIMEOUT_MS;
  const hops = opts.hops || 0;
  const proxy = opts.proxy === undefined ? proxyFor(url, opts.env) : opts.proxy;
  const requestOptions = proxy ? { createConnection: tunnelThrough(proxy, url, timeoutMs) } : {};

  return new Promise((resolve, reject) => {
    let finalResponse = null;
    const request = get(url, requestOptions, (response) => {
      const status = response.statusCode;

      if (REDIRECT_STATUSES.includes(status)) {
        response.resume();
        const location = response.headers.location;
        if (!location) {
          reject(permanent(`Redirect (HTTP ${status}) without a Location header from ${url}`));
          return;
        }
        if (hops >= MAX_REDIRECTS) {
          reject(permanent(`Too many redirects (more than ${MAX_REDIRECTS}) downloading ${url}`));
          return;
        }
        const next = new URL(location, url).toString();
        resolve(
          getFollowingRedirects(next, { get, timeoutMs, proxy: opts.proxy, env: opts.env, hops: hops + 1 })
        );
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
// tries. An error flagged `retryable: false` (a 404, a redirect loop) is
// thrown at once.
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

// Extract a verified archive into `dir`. `tar` gets an argv, never a command
// line: a quote or `$(...)` in the install path is not interpreted by a shell.
function extractArchive(archivePath, dir) {
  execFileSync('tar', ['-xzf', archivePath, '-C', dir], { stdio: 'inherit' });
}

// Download, verify and extract the binary. `options` (tests) override the
// release URL (`baseUrl`), the install directory (`binDir`), the platform
// (`platform`, `arch`, `libc`), the transport (`get`, `proxy`) and the log.
async function install(options) {
  const opts = options || {};
  const log = opts.log || ((message) => console.log(message));
  const { target, isWindows } = getPlatform(
    opts.platform || process.platform,
    opts.arch || process.arch,
    opts.libc === undefined ? detectLibc() : opts.libc
  );
  const binName = isWindows ? 'semantiq.exe' : 'semantiq';
  const archiveName = `semantiq-v${VERSION}-${target}.tar.gz`;
  const url = `${opts.baseUrl || RELEASE_BASE_URL}/${archiveName}`;
  const checksumUrl = `${url}.sha256`;
  const transport = { get: opts.get, proxy: opts.proxy, timeoutMs: opts.timeoutMs };
  const retry = { baseDelayMs: opts.retryDelayMs, log: opts.retryLog };

  const binDir = opts.binDir || path.join(__dirname, '..', 'bin');
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

  log(`Downloading Semantiq v${VERSION} for ${target}...`);
  if (transport.proxy === undefined) {
    const proxy = proxyFor(url);
    if (proxy) log(`Using proxy ${new URL(proxy).host}`);
  }

  try {
    await withRetry('Download', () => downloadFile(url, archivePath, transport), retry);

    // Integrity verification: never extract/execute an unverified binary.
    // The release CI publishes "<archive>.sha256" next to each artifact.
    let expectedSha;
    try {
      const checksumRaw = await withRetry(
        'Checksum download',
        () => downloadText(checksumUrl, transport),
        retry
      );
      expectedSha = parseExpectedSha256(checksumRaw, archiveName);
    } catch (err) {
      throw new Error(
        `Could not verify integrity (missing or unreadable ${archiveName}.sha256): ${err.message}`
      );
    }

    const actualSha = sha256File(archivePath);
    if (actualSha !== expectedSha) {
      throw new Error(
        `Checksum mismatch for ${archiveName}\n  expected: ${expectedSha}\n  actual:   ${actualSha}`
      );
    }

    log('Checksum verified.');

    // Extract (verified archive only)
    extractArchive(archivePath, binDir);
    if (!isWindows) fs.chmodSync(binPath, 0o755);

    // Cleanup
    fs.unlinkSync(archivePath);
  } catch (error) {
    removeArtifacts();
    throw error;
  }

  log('Semantiq installed successfully!');
  log('Next: run `semantiq init` in your project (installs the Claude Code skill + MCP server and indexes it),');
  log('or `semantiq init --global` to install the skill for all your projects.');
  return binPath;
}

if (require.main === module) {
  install().catch((error) => {
    console.error('Failed to install Semantiq:', error.message);
    console.error('');
    console.error('Alternative installation methods:');
    console.error(`  download a release archive: https://github.com/${REPO}/releases/tag/v${VERSION}`);
    console.error(`  build from source: cargo install --git https://github.com/${REPO}.git`);
    process.exit(1);
  });
} else {
  module.exports = {
    MAX_REDIRECTS,
    PLATFORMS,
    HttpStatusError,
    detectLibc,
    getPlatform,
    proxyFor,
    tunnelThrough,
    getFollowingRedirects,
    downloadFile,
    downloadText,
    withRetry,
    parseExpectedSha256,
    extractArchive,
    install,
  };
}
