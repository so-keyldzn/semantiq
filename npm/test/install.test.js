// Tests of the download helpers of scripts/install.js against a local HTTP
// server (`npm test`, Node 18+ for `node:test`). The installer itself is not
// run: it would download a release.

const test = require('node:test');
const assert = require('node:assert/strict');
const http = require('http');
const fs = require('fs');
const os = require('os');
const path = require('path');

const {
  MAX_REDIRECTS,
  HttpStatusError,
  getFollowingRedirects,
  downloadFile,
  downloadText,
  withRetry,
  parseExpectedSha256,
} = require('../scripts/install.js');

const BODY = 'release archive bytes';

// Routes: /file (200), /redirect-<status> (to /file), /chain (MAX_REDIRECTS
// hops to /file), /loop (redirects to itself), /no-location, /missing (404),
// /flaky (503 twice, then 200), /stall (never answers), /slow-body (200 whose
// body never ends).
function startServer() {
  const requests = [];
  let flakyCalls = 0;
  const server = http.createServer((req, res) => {
    requests.push(req.url);
    const url = new URL(req.url, 'http://localhost');
    const redirect = (status, location) => {
      res.writeHead(status, { Location: location });
      res.end('redirecting');
    };
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
    await assert.rejects(downloadText(`${server.base}/loop`, opts()), /Too many redirects/);
    assert.equal(
      server.requests.filter((u) => u === '/loop').length,
      MAX_REDIRECTS + 1,
      'the first request plus MAX_REDIRECTS hops'
    );
    await assert.rejects(
      downloadText(`${server.base}/no-location`, opts()),
      /without a Location header/
    );
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
