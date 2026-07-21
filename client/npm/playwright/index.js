'use strict';

/**
 * @portzero/playwright — Playwright fixtures for Port Zero tunnels.
 *
 * Makes tunnel-backed apps first-class in Playwright:
 *   - resolves `baseURL` by asking the daemon (via the `portzero` CLI),
 *   - waits for tunnel readiness / health before tests start,
 *   - supplies `httpCredentials` for access-controlled (cloud) tunnels,
 *   - optionally stamps `X-PZ-Test: <test title>` so the daemon can attribute
 *     exercised routes per test.
 *
 * The same fixture works in local dev and in GitHub Actions — that is the point.
 * It talks to the daemon through the shipped `portzero` binary, so there is no
 * private protocol to keep in sync.
 */

const { execFile } = require('node:child_process');

/** Name of the CLI binary; override with PORTZERO_BIN for non-standard installs. */
function portzeroBin() {
  return process.env.PORTZERO_BIN || 'portzero';
}

/** A clear, actionable error when the daemon/CLI is unavailable. */
class PortzeroError extends Error {
  constructor(message) {
    super(message);
    this.name = 'PortzeroError';
  }
}

function runPortzero(args, { timeoutMs } = {}) {
  return new Promise((resolve, reject) => {
    execFile(
      portzeroBin(),
      args,
      { timeout: timeoutMs, encoding: 'utf8' },
      (err, stdout, stderr) => {
        if (err) {
          if (err.code === 'ENOENT') {
            reject(
              new PortzeroError(
                `Could not run '${portzeroBin()}'. Is the Port Zero CLI installed and on PATH? ` +
                  `Set PORTZERO_BIN to its absolute path if it is installed elsewhere. ` +
                  `See https://portzero.net.`,
              ),
            );
            return;
          }
          const detail = (stderr || stdout || err.message).toString().trim();
          reject(
            new PortzeroError(
              `\`${portzeroBin()} ${args.join(' ')}\` failed: ${detail}\n` +
                `Is the daemon running (\`portzero status\`) and is PZ_TUNNEL set on the ` +
                `process/container?`,
            ),
          );
          return;
        }
        resolve(stdout.toString());
      },
    );
  });
}

/**
 * Resolve a tunnel domain to its base URL by asking the daemon.
 * Throws a {@link PortzeroError} with a clear message if the tunnel is unknown
 * or the daemon is not running.
 *
 * @param {string} domain e.g. "web.myapp.portzero.local"
 * @returns {Promise<string>} e.g. "http://web.myapp.portzero.local"
 */
async function resolveBaseUrl(domain) {
  if (!domain) throw new PortzeroError('resolveBaseUrl: a tunnel domain is required');
  const out = await runPortzero(['url', domain]);
  const url = out.trim();
  if (!url) {
    throw new PortzeroError(`No URL returned for tunnel '${domain}'.`);
  }
  return url;
}

/**
 * Block until a tunnel is up (and, when `healthy`, until its health path returns
 * 2xx). Delegates to `portzero wait`.
 *
 * @param {string} domain
 * @param {{ healthy?: boolean, timeoutSecs?: number }} [opts]
 * @returns {Promise<void>}
 */
async function waitForTunnel(domain, opts = {}) {
  if (!domain) throw new PortzeroError('waitForTunnel: a tunnel domain is required');
  const args = ['wait', domain];
  if (opts.healthy) args.push('--healthy');
  if (opts.timeoutSecs != null) args.push('--timeout', String(opts.timeoutSecs));
  // Give the child a little more wall-clock than its own --timeout so we surface
  // the CLI's own clear timeout message rather than a generic kill.
  const timeoutMs =
    opts.timeoutSecs != null ? (opts.timeoutSecs + 10) * 1000 : 70 * 1000;
  await runPortzero(args, { timeoutMs });
}

/**
 * Build a Playwright `webServer` fragment that gates the run on a tunnel being
 * up. Handy for `playwright.config.ts` when you prefer `webServer` over the
 * fixtures.
 *
 * @param {string} domain
 * @param {{ command?: string, healthy?: boolean, timeoutSecs?: number, reuseExistingServer?: boolean }} [opts]
 */
function webServerFor(domain, opts = {}) {
  const waitCmd = [
    portzeroBin(),
    'wait',
    domain,
    opts.healthy === false ? '' : '--healthy',
    opts.timeoutSecs != null ? `--timeout ${opts.timeoutSecs}` : '',
  ]
    .filter(Boolean)
    .join(' ');
  const command = opts.command ? `${opts.command} && ${waitCmd}` : waitCmd;
  return {
    command,
    url: undefined, // resolved lazily; prefer the baseURL fixture below
    reuseExistingServer:
      opts.reuseExistingServer != null ? opts.reuseExistingServer : !process.env.CI,
    timeout: (opts.timeoutSecs != null ? opts.timeoutSecs + 15 : 120) * 1000,
  };
}

/**
 * Read httpCredentials for an access-controlled (cloud) tunnel from the
 * environment (`PZ_HTTP_USERNAME` / `PZ_HTTP_PASSWORD`), or `undefined`.
 */
function credentialsFromEnv() {
  const username = process.env.PZ_HTTP_USERNAME;
  const password = process.env.PZ_HTTP_PASSWORD;
  if (username && password) return { username, password };
  return undefined;
}

/**
 * Create the extended Playwright `test` with Port Zero fixtures.
 *
 * We take `@playwright/test` as an argument so this package does not hard-depend
 * on a specific Playwright install (it is a peer dependency). Most users import
 * the ready-made {@link test} export below instead.
 *
 * Options (set via `test.use({ ... })`):
 *   - `portzeroTunnel`: tunnel domain to resolve `baseURL` from.
 *   - `portzeroWaitHealthy`: also wait for the health path (default true).
 *   - `portzeroWaitTimeoutSecs`: wait timeout (default: CLI default).
 *   - `portzeroTestHeader`: stamp `X-PZ-Test: <title>` (default true).
 */
function createTest(base) {
  return base.test.extend({
    portzeroTunnel: [process.env.PORTZERO_TUNNEL || undefined, { option: true }],
    portzeroWaitHealthy: [true, { option: true }],
    portzeroWaitTimeoutSecs: [undefined, { option: true }],
    portzeroTestHeader: [true, { option: true }],

    // Resolve baseURL from the tunnel (after waiting for readiness).
    baseURL: async (
      { baseURL, portzeroTunnel, portzeroWaitHealthy, portzeroWaitTimeoutSecs },
      use,
    ) => {
      if (portzeroTunnel) {
        await waitForTunnel(portzeroTunnel, {
          healthy: portzeroWaitHealthy,
          timeoutSecs: portzeroWaitTimeoutSecs,
        });
        await use(await resolveBaseUrl(portzeroTunnel));
      } else {
        await use(baseURL);
      }
    },

    // Supply httpCredentials from the environment for access-controlled tunnels,
    // falling back to whatever the config already set.
    httpCredentials: async ({ httpCredentials }, use) => {
      await use(credentialsFromEnv() || httpCredentials);
    },

    // Stamp X-PZ-Test with the test title so the daemon can attribute exercised
    // routes per test.
    extraHTTPHeaders: async ({ extraHTTPHeaders, portzeroTestHeader }, use, testInfo) => {
      const headers = { ...(extraHTTPHeaders || {}) };
      if (portzeroTestHeader && testInfo && testInfo.title) {
        headers['X-PZ-Test'] = testInfo.title;
      }
      await use(headers);
    },
  });
}

// Lazily build the ready-made `test`/`expect` exports. Requiring
// `@playwright/test` lazily keeps this module importable (e.g. for
// `resolveBaseUrl`) even in a context where Playwright is not installed.
let _base = null;
function base() {
  if (_base) return _base;
  try {
    // eslint-disable-next-line global-require
    _base = require('@playwright/test');
  } catch (e) {
    throw new PortzeroError(
      "Cannot find '@playwright/test'. @portzero/playwright is a thin layer on top of " +
        'Playwright — install it as a peer dependency: `npm i -D @playwright/test`.',
    );
  }
  return _base;
}

module.exports = {
  PortzeroError,
  resolveBaseUrl,
  waitForTunnel,
  webServerFor,
  credentialsFromEnv,
  createTest,
  /** Extended Playwright `test` with Port Zero fixtures. */
  get test() {
    return createTest(base());
  },
  /** Re-exported Playwright `expect`. */
  get expect() {
    return base().expect;
  },
};
