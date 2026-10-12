// Tests of scripts/install.js against a local HTTP server (`npm test`, Node
// 18+ for `node:test`): the download helpers, the proxy tunnel and the
// installer itself, pointed at a release archive served locally (nothing is
// downloaded from GitHub).

const test = require('node:test');
const assert = require('node:assert/strict');
const crypto = require('crypto');
const http = require('http');
const net = require('net');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');

const {
  MAX_REDIRECTS,
  PLATFORMS,
  HttpStatusError,
  getPlatform,
  proxyFor,
  getFollowingRedirects,
  downloadFile,
  downloadText,
  withRetry,
  parseExpectedSha256,
  install,
} = require('../scripts/install.js');

const VERSION = require('../package.json').version;
const BODY = 'release archive bytes';

// Routes: /file (200), /redirect-<status> (to /file), /chain (MAX_REDIRECTS
// hops to /file), /loop (redirects to itself), /no-location, /missing (404),
// /flaky (503 twice, then 200), /stall (never answers), /slow-body (200 whose
// body never ends). `files` maps further paths to Buffers served with 200.
function startServer(files) {
  const requests = [];
  let flakyCalls = 0;
  const server = http.createServer((req, res) => {
    requests.push(req.url);
    const url = new URL(req.url, 'http://localhost');
    const redirect = (status, location) => {
      res.writeHead(status, { Location: location });
      res.end('redirecting');
    };
    if (files && files[url.pathname]) {
      res.writeHead(200, { 'Content-Type': 'application/octet-stream' });
      res.end(files[url.pathname]);
      return;
    }
    switch (url.pathname) {
      case '/file':
        res.writeHead(200, { 'Content-Type': 'application/octet-stream' });
        res.end(BODY);
        return;
      case '/redirect-301':
      case '/redirect-302':
      case '/redirect-303':
      case '/redirect-307':
      case '/redirect-308':
        redirect(Number(url.pathname.slice('/redirect-'.length)), '/file');
        return;
      case '/chain': {
        const hop = Number(url.searchParams.get('hop') || 0);
        redirect(307, hop + 1 >= MAX_REDIRECTS ? '/file' : `/chain?hop=${hop + 1}`);
        return;
      }
      case '/loop':
        redirect(302, '/loop');
        return;
      case '/no-location':
        res.writeHead(302);
        res.end();
        return;
      case '/missing':
        res.writeHead(404);
        res.end('not found');
        return;
      case '/flaky':
        flakyCalls += 1;
        if (flakyCalls <= 2) {
          res.writeHead(503);
          res.end('try later');
        } else {
          res.writeHead(200);
          res.end(BODY);
        }
        return;
      case '/stall':
        // Never answer: the client must time out.
        return;
      case '/slow-body':
        res.writeHead(200);
        res.write('partial');
        return;
      default:
        res.writeHead(500);
        res.end('unexpected route');
    }
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const base = `http://127.0.0.1:${server.address().port}`;
      resolve({
        base,
        requests,
        close: () => new Promise((done) => server.close(done)),
      });
    });
  });
}

const opts = (extra) => Object.assign({ get: http.get }, extra);

test('downloadText follows 301, 302, 303, 307 and 308 redirects', async () => {
  const server = await startServer();
  try {
    for (const status of [301, 302, 303, 307, 308]) {
      const text = await downloadText(`${server.base}/redirect-${status}`, opts());
      assert.equal(text, BODY, `HTTP ${status}`);
    }
    assert.equal(await downloadText(`${server.base}/chain`, opts()), BODY);
  } finally {
    await server.close();
  }
});

test('redirect loops and redirects without Location are errors', async () => {
  const server = await startServer();
  try {
    await assert.rejects(
      downloadText(`${server.base}/loop`, opts()),
      (err) => /Too many redirects/.test(err.message) && err.retryable === false
    );
    assert.equal(
      server.requests.filter((u) => u === '/loop').length,
      MAX_REDIRECTS + 1,
      'the first request plus MAX_REDIRECTS hops'
    );
    await assert.rejects(
      downloadText(`${server.base}/no-location`, opts()),
      (err) => /without a Location header/.test(err.message) && err.retryable === false
    );
    // Neither is retried.
    const before = server.requests.length;
    await assert.rejects(
      withRetry('Download', () => downloadText(`${server.base}/no-location`, opts()), {
        baseDelayMs: 1,
        log: () => {},
      }),
      /without a Location header/
    );
    assert.equal(server.requests.length, before + 1);
  } finally {
    await server.close();
  }
});

test('downloadFile writes the body and removes a partial file on failure', async () => {
  const server = await startServer();
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'semantiq-install-'));
  const dest = path.join(dir, 'archive.tar.gz');
  try {
    await downloadFile(`${server.base}/redirect-307`, dest, opts());
    assert.equal(fs.readFileSync(dest, 'utf8'), BODY);
    fs.unlinkSync(dest);

    await assert.rejects(
      downloadFile(`${server.base}/missing`, dest, opts()),
      (err) => err instanceof HttpStatusError && err.statusCode === 404 && err.retryable === false
    );
    assert.equal(fs.existsSync(dest), false);

    await assert.rejects(
      downloadFile(`${server.base}/slow-body`, dest, opts({ timeoutMs: 200 })),
      /Timed out after 0.2s/
    );
    assert.equal(fs.existsSync(dest), false, 'partial download removed');
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
    await server.close();
  }
});

test('a stalled connection times out instead of hanging', async () => {
  const server = await startServer();
  try {
    await assert.rejects(
      getFollowingRedirects(`${server.base}/stall`, opts({ timeoutMs: 200 })),
      /Timed out after 0.2s waiting for/
    );
  } finally {
    await server.close();
  }
});

test('withRetry retries transient failures with backoff and gives up after the attempts', async () => {
  const server = await startServer();
  const logs = [];
  try {
    const text = await withRetry(
      'Download',
      () => downloadText(`${server.base}/flaky`, opts()),
      { baseDelayMs: 1, log: (m) => logs.push(m) }
    );
    assert.equal(text, BODY);
    assert.equal(server.requests.filter((u) => u === '/flaky').length, 3);
    assert.equal(logs.length, 2);
    assert.match(logs[0], /HTTP 503.*retrying in 0.001s \(attempt 2\/3\)/);
    assert.match(logs[1], /retrying in 0.002s \(attempt 3\/3\)/);

    // 404 is final: no retry.
    await assert.rejects(
      withRetry('Download', () => downloadText(`${server.base}/missing`, opts()), {
        baseDelayMs: 1,
        log: () => {},
      }),
      /HTTP 404/
    );
    assert.equal(server.requests.filter((u) => u === '/missing').length, 1);

    // A network error is retried up to the attempt count, then thrown.
    let calls = 0;
    await assert.rejects(
      withRetry(
        'Download',
        () => {
          calls += 1;
          return Promise.reject(new Error('ECONNRESET'));
        },
        { attempts: 3, baseDelayMs: 1, log: () => {} }
      ),
      /ECONNRESET/
    );
    assert.equal(calls, 3);
  } finally {
    await server.close();
  }
});

test('parseExpectedSha256 accepts bare and sha256sum formats', () => {
  const hex = 'a'.repeat(64);
  assert.equal(parseExpectedSha256(hex, 'x.tar.gz'), hex);
  assert.equal(parseExpectedSha256(`${hex.toUpperCase()}  x.tar.gz\n`, 'x.tar.gz'), hex);
  assert.throws(() => parseExpectedSha256('not a digest', 'x.tar.gz'), /Malformed checksum/);
});

test('getPlatform maps the served platforms and refuses the others', () => {
  const cases = {
    'darwin-x64': 'x86_64-apple-darwin',
    'darwin-arm64': 'aarch64-apple-darwin',
    'linux-x64': 'x86_64-unknown-linux-gnu',
    'linux-arm64': 'aarch64-unknown-linux-gnu',
    'win32-x64': 'x86_64-pc-windows-msvc',
  };
  assert.deepEqual(Object.keys(PLATFORMS).sort(), Object.keys(cases).sort());
  for (const [key, target] of Object.entries(cases)) {
    const [platform, arch] = key.split('-');
    assert.deepEqual(getPlatform(platform, arch, null), { target, isWindows: platform === 'win32' });
  }
  assert.deepEqual(getPlatform('linux', 'x64', 'glibc').target, 'x86_64-unknown-linux-gnu');

  // npm filters os and cpu separately: these pass its check, not ours.
  for (const [platform, arch] of [['win32', 'arm64'], ['linux', 'ia32'], ['freebsd', 'x64']]) {
    assert.throws(
      () => getPlatform(platform, arch, null),
      (err) => /^Unsupported platform: /.test(err.message) && /darwin-x64, .*win32-x64/.test(err.message) && err.retryable === false,
      `${platform}-${arch}`
    );
  }
  assert.throws(() => getPlatform('linux', 'x64', 'musl'), /musl \(Alpine\).*cargo install --git/s);
  assert.throws(() => getPlatform('linux', 'arm64', 'musl'), /musl/);
});

test('proxyFor reads the npm and environment proxy settings', () => {
  const url = 'https://github.com/so-keyldzn/semantiq/releases/download/v1/x.tar.gz';
  assert.equal(proxyFor(url, {}), null);
  assert.equal(proxyFor(url, { HTTPS_PROXY: 'http://proxy:3128' }), 'http://proxy:3128');
  assert.equal(proxyFor(url, { https_proxy: 'http://lower:3128' }), 'http://lower:3128');
  assert.equal(proxyFor(url, { ALL_PROXY: 'http://all:3128' }), 'http://all:3128');
  // npm's own config wins over the environment, https-proxy over proxy.
  assert.equal(
    proxyFor(url, { npm_config_proxy: 'http://npm:1', HTTPS_PROXY: 'http://env:2' }),
    'http://npm:1'
  );
  assert.equal(
    proxyFor(url, { npm_config_https_proxy: 'http://npms:1', npm_config_proxy: 'http://npm:1' }),
    'http://npms:1'
  );
  // no_proxy: exact host, parent domain (with or without a leading dot), `*`.
  const env = { HTTPS_PROXY: 'http://proxy:3128' };
  assert.equal(proxyFor(url, { ...env, NO_PROXY: 'github.com' }), null);
  assert.equal(proxyFor(url, { ...env, no_proxy: 'example.org, .github.com' }), null);
  assert.equal(proxyFor(url, { ...env, npm_config_noproxy: '*' }), null);
  assert.equal(proxyFor(url, { ...env, NO_PROXY: 'example.org' }), 'http://proxy:3128');
  assert.equal(proxyFor(url, { ...env, NO_PROXY: 'hub.com' }), 'http://proxy:3128', 'suffix is not a domain match');
});

// An HTTP proxy that tunnels CONNECT requests to the local test server, or
// refuses them with `refuseStatus`. Records the CONNECT requests.
function startProxy(refuseStatus) {
  const connects = [];
  const proxy = http.createServer((req, res) => {
    res.writeHead(405);
    res.end('CONNECT only');
  });
  proxy.on('connect', (req, clientSocket, head) => {
    connects.push({ url: req.url, headers: req.headers });
    if (refuseStatus) {
      clientSocket.end(`HTTP/1.1 ${refuseStatus} Forbidden\r\n\r\n`);
      return;
    }
    const [host, port] = req.url.split(':');
    const upstream = net.connect(Number(port), host, () => {
      clientSocket.write('HTTP/1.1 200 Connection Established\r\n\r\n');
      upstream.write(head);
      upstream.pipe(clientSocket);
      clientSocket.pipe(upstream);
    });
    upstream.on('error', () => clientSocket.destroy());
    clientSocket.on('error', () => upstream.destroy());
  });
  return new Promise((resolve) => {
    proxy.listen(0, '127.0.0.1', () => {
      resolve({
        port: proxy.address().port,
        connects,
        close: () => new Promise((done) => proxy.close(done)),
      });
    });
  });
}

test('downloads tunnel through the configured proxy with CONNECT', async () => {
  const server = await startServer();
  const proxy = await startProxy();
  try {
    const proxyUrl = `http://user%40corp:s%3Acret@127.0.0.1:${proxy.port}`;
    const text = await downloadText(`${server.base}/redirect-302`, opts({ proxy: proxyUrl }));
    assert.equal(text, BODY);
    // One tunnel per hop, each addressed to the target, not the proxy.
    const targetHost = server.base.replace('http://', '');
    assert.equal(proxy.connects.length, 2);
    assert.equal(proxy.connects[0].url, targetHost);
    assert.equal(proxy.connects[0].headers.host, targetHost);
    assert.equal(
      proxy.connects[0].headers['proxy-authorization'],
      `Basic ${Buffer.from('user@corp:s:cret').toString('base64')}`
    );
    assert.deepEqual(server.requests, ['/redirect-302', '/file']);

    // `proxy: false` (or no proxy configured) goes direct.
    assert.equal(await downloadText(`${server.base}/file`, opts({ proxy: false })), BODY);
    assert.equal(proxy.connects.length, 2);
  } finally {
    await proxy.close();
    await server.close();
  }
});

test('a proxy that refuses the tunnel or cannot be reached is an error', async () => {
  const server = await startServer();
  const refusing = await startProxy(403);
  try {
    await assert.rejects(
      downloadText(`${server.base}/file`, opts({ proxy: `http://127.0.0.1:${refusing.port}` })),
      (err) => /refused CONNECT .*HTTP 403/.test(err.message) && err.retryable === false
    );
    assert.equal(server.requests.length, 0);

    const closed = await startProxy();
    await closed.close();
    await assert.rejects(
      downloadText(`${server.base}/file`, opts({ proxy: `http://127.0.0.1:${closed.port}` })),
      /Proxy 127.0.0.1:\d+: connect ECONNREFUSED/
    );
  } finally {
    await refusing.close();
    await server.close();
  }
});

// A release archive (`semantiq-v<version>-<target>.tar.gz` holding one file
// named `binName`) and its `.sha256` in the `sha256sum` format.
function releaseFiles(target, binName) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'semantiq-release-'));
  try {
    fs.writeFileSync(path.join(dir, binName), '#!/bin/sh\necho semantiq\n');
    const archiveName = `semantiq-v${VERSION}-${target}.tar.gz`;
    execFileSync('tar', ['-czf', path.join(dir, archiveName), '-C', dir, binName]);
    const archive = fs.readFileSync(path.join(dir, archiveName));
    const digest = crypto.createHash('sha256').update(archive).digest('hex');
    return {
      archiveName,
      [`/${archiveName}`]: archive,
      [`/${archiveName}.sha256`]: Buffer.from(`${digest}  ${archiveName}\n`),
    };
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

// An install directory whose name a shell would interpret.
function hostileBinDir() {
  const name = process.platform === 'win32' ? '%PATH% semantiq bin' : `it's "a" $(echo pwned) bin`;
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'semantiq-install-'));
  const binDir = path.join(root, name);
  fs.mkdirSync(binDir);
  return { root, binDir };
}

test('install downloads, verifies, extracts and cleans up, whatever the install path', async () => {
  const platform = { platform: 'linux', arch: 'x64', libc: 'glibc' };
  const files = releaseFiles('x86_64-unknown-linux-gnu', 'semantiq');
  const server = await startServer(files);
  const { root, binDir } = hostileBinDir();
  const logs = [];
  try {
    const binPath = await install({
      ...platform,
      baseUrl: server.base,
      binDir,
      get: http.get,
      proxy: false,
      log: (m) => logs.push(m),
    });
    assert.equal(binPath, path.join(binDir, 'semantiq'));
    assert.equal(fs.readFileSync(binPath, 'utf8'), '#!/bin/sh\necho semantiq\n');
    if (process.platform !== 'win32') {
      assert.equal(fs.statSync(binPath).mode & 0o777, 0o755);
    }
    assert.deepEqual(fs.readdirSync(binDir), ['semantiq'], 'the archive is removed');
    assert.deepEqual(server.requests, [`/${files.archiveName}`, `/${files.archiveName}.sha256`]);
    assert.match(logs[0], /^Downloading Semantiq v.* for x86_64-unknown-linux-gnu/);
    assert.ok(logs.includes('Checksum verified.'));
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
    await server.close();
  }
});

test('install rejects a checksum mismatch and an unsupported platform without leaving files', async () => {
  const files = releaseFiles('aarch64-apple-darwin', 'semantiq');
  files[`/${files.archiveName}.sha256`] = Buffer.from(`${'0'.repeat(64)}  ${files.archiveName}\n`);
  const server = await startServer(files);
  const { root, binDir } = hostileBinDir();
  const common = { baseUrl: server.base, binDir, get: http.get, proxy: false, log: () => {}, retryDelayMs: 1, retryLog: () => {} };
  try {
    await assert.rejects(
      install({ ...common, platform: 'darwin', arch: 'arm64' }),
      /Checksum mismatch for semantiq-v.*-aarch64-apple-darwin\.tar\.gz/
    );
    assert.deepEqual(fs.readdirSync(binDir), []);

    // A missing .sha256 is fatal too (the 404 is not retried).
    delete files[`/${files.archiveName}.sha256`];
    await assert.rejects(
      install({ ...common, platform: 'darwin', arch: 'arm64' }),
      /Could not verify integrity .*HTTP 500/
    );
    assert.deepEqual(fs.readdirSync(binDir), []);

    const before = server.requests.length;
    await assert.rejects(install({ ...common, platform: 'win32', arch: 'arm64' }), /Unsupported platform: win32-arm64/);
    await assert.rejects(install({ ...common, platform: 'linux', arch: 'x64', libc: 'musl' }), /musl/);
    assert.equal(server.requests.length, before, 'nothing downloaded');
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
    await server.close();
  }
});
