const assert = require('node:assert/strict');
const { execFile, execFileSync } = require('node:child_process');
const { once } = require('node:events');
const fs = require('node:fs');
const https = require('node:https');
const os = require('node:os');
const path = require('node:path');
const { promisify } = require('node:util');

async function main() {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'pinned-dist-harness-'));
  let server;
  try {
    const key = path.join(temp, 'key.pem');
    const cert = path.join(temp, 'cert.pem');
    execFileSync('openssl', [
      'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
      '-subj', '/CN=github.com', '-addext', 'subjectAltName=DNS:github.com',
      '-keyout', key, '-out', cert,
    ], { stdio: 'pipe' });

    let requests = 0;
    server = https.createServer({ key: fs.readFileSync(key), cert: fs.readFileSync(cert) }, (_request, response) => {
      requests += 1;
      response.writeHead(requests === 1 ? 503 : 200);
      response.end('pinned dist download fixture');
    });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');

    // Use curl's implicit config to route and trust the HTTPS fixture and to
    // enable the retry that the installer must override.
    fs.writeFileSync(path.join(temp, '.curlrc'), [
      `cacert = ${JSON.stringify(cert)}`,
      `connect-to = "github.com:443:127.0.0.1:${server.address().port}"`,
      'noproxy = "*"',
      'max-time = 5',
      'retry = 1',
      '',
    ].join('\n'));

    const result = await promisify(execFile)('bash', [
      'scripts/install-pinned-dist.sh', '--dest', path.join(temp, 'dist'),
    ], { env: { ...process.env, CURL_HOME: temp }, timeout: 15000 })
      .then(({ stderr }) => ({ code: 0, stderr }), (error) => error);

    assert.equal(requests, 1, `expected one download request, got ${requests}\n${result.stderr}`);
    assert.equal(result.code, 22, result.stderr);
    assert.match(result.stderr, /\b503\b/);
    console.log('install-pinned-dist-harness: ok');
  } finally {
    server?.close();
    fs.rmSync(temp, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
