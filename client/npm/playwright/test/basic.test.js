'use strict';

const { test } = require('node:test');
const assert = require('node:assert');

const pz = require('../index.js');

test('credentialsFromEnv reads PZ_HTTP_* or returns undefined', () => {
  delete process.env.PZ_HTTP_USERNAME;
  delete process.env.PZ_HTTP_PASSWORD;
  assert.strictEqual(pz.credentialsFromEnv(), undefined);

  process.env.PZ_HTTP_USERNAME = 'alice';
  process.env.PZ_HTTP_PASSWORD = 's3cret';
  assert.deepStrictEqual(pz.credentialsFromEnv(), {
    username: 'alice',
    password: 's3cret',
  });
  delete process.env.PZ_HTTP_USERNAME;
  delete process.env.PZ_HTTP_PASSWORD;
});

test('webServerFor builds a wait command that gates on the tunnel', () => {
  const ws = pz.webServerFor('web.myapp.portzero.local', {
    command: 'docker compose up -d',
    timeoutSecs: 90,
  });
  assert.match(ws.command, /docker compose up -d && /);
  assert.match(ws.command, /wait web\.myapp\.portzero\.local/);
  assert.match(ws.command, /--healthy/);
  assert.match(ws.command, /--timeout 90/);
  assert.strictEqual(typeof ws.timeout, 'number');
});

test('webServerFor can disable the health wait', () => {
  const ws = pz.webServerFor('web.myapp.portzero.local', { healthy: false });
  assert.doesNotMatch(ws.command, /--healthy/);
});

test('resolveBaseUrl degrades clearly when the CLI is missing', async () => {
  const prev = process.env.PORTZERO_BIN;
  process.env.PORTZERO_BIN = 'portzero-does-not-exist-xyz';
  try {
    await assert.rejects(
      () => pz.resolveBaseUrl('web.myapp.portzero.local'),
      (err) => {
        assert.strictEqual(err.name, 'PortzeroError');
        assert.match(err.message, /Port Zero CLI|PATH|installed/i);
        return true;
      },
    );
  } finally {
    if (prev === undefined) delete process.env.PORTZERO_BIN;
    else process.env.PORTZERO_BIN = prev;
  }
});

test('waitForTunnel requires a domain', async () => {
  await assert.rejects(() => pz.waitForTunnel(''), /domain is required/);
});
