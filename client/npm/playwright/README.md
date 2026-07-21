# @portzero/playwright

Playwright fixtures for [Port Zero](https://portzero.net) tunnels. Makes
tunnel-backed apps first-class in Playwright:

- resolves `baseURL` by asking the daemon (via the `portzero` CLI),
- waits for tunnel readiness / health before tests start,
- supplies `httpCredentials` for access-controlled (cloud) tunnels,
- optionally stamps `X-PZ-Test: <test title>` so the daemon can attribute
  exercised routes per test.

The **same** fixture works in local dev and in GitHub Actions — that is the
point. It talks to the daemon through the shipped `portzero` binary, so there is
no private protocol to keep in sync.

## Install

```bash
npm i -D @portzero/playwright @playwright/test
```

`@playwright/test` is a peer dependency. The `portzero` CLI must be installed and
on `PATH` (set `PORTZERO_BIN` to override the path).

## Usage — fixtures (recommended)

```ts
// example.spec.ts
import { test, expect } from '@portzero/playwright';

// Resolve baseURL from this tunnel, waiting for its health path first.
test.use({ portzeroTunnel: 'web.myapp.portzero.local' });

test('home page loads', async ({ page }) => {
  await page.goto('/');                 // baseURL came from the daemon
  await expect(page).toHaveTitle(/My App/);
});
```

Options (via `test.use({ ... })` or the `PORTZERO_TUNNEL` env var):

| Option                     | Default | Meaning                                             |
|----------------------------|---------|-----------------------------------------------------|
| `portzeroTunnel`           | `PORTZERO_TUNNEL` | Tunnel domain to resolve `baseURL` from.  |
| `portzeroWaitHealthy`      | `true`  | Also wait for the health path before tests.         |
| `portzeroWaitTimeoutSecs`  | CLI default | Readiness wait timeout.                         |
| `portzeroTestHeader`       | `true`  | Stamp `X-PZ-Test: <title>` on requests.             |

`httpCredentials` are read from `PZ_HTTP_USERNAME` / `PZ_HTTP_PASSWORD` when set,
for access-controlled cloud tunnels.

## Usage — `webServer` (config-only)

If you prefer to gate the whole run in `playwright.config.ts`:

```ts
import { defineConfig } from '@playwright/test';
import { webServerFor } from '@portzero/playwright';

export default defineConfig({
  webServer: webServerFor('web.myapp.portzero.local', {
    command: 'docker compose up -d',
    healthy: true,
  }),
  use: { baseURL: 'http://web.myapp.portzero.local' },
});
```

## Programmatic helpers

```ts
import { resolveBaseUrl, waitForTunnel } from '@portzero/playwright';

await waitForTunnel('web.myapp.portzero.local', { healthy: true });
const baseURL = await resolveBaseUrl('web.myapp.portzero.local');
```

## Degrades clearly

If the daemon is not running, the tunnel is unknown, or the `portzero` CLI is not
on `PATH`, the helpers throw a `PortzeroError` with an actionable message (run
`portzero status`, set `PZ_TUNNEL`, install the CLI) instead of hanging or
failing obscurely.

## GitHub Actions

The identical fixture runs in CI. See
[`example/github-actions.example.yml`](./example/github-actions.example.yml).
