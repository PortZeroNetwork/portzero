// Example spec. Import `test`/`expect` from @portzero/playwright instead of
// @playwright/test to get the tunnel fixtures.
import { test, expect } from '@portzero/playwright';

// Resolve baseURL from this tunnel, waiting for its health path first. You can
// also set this once in playwright.config.ts via `use`, or with PORTZERO_TUNNEL.
test.use({ portzeroTunnel: process.env.PORTZERO_TUNNEL || 'web.myapp.portzero.local' });

test('home page loads over the tunnel', async ({ page }) => {
  // No hardcoded host/port: baseURL came from the daemon. Each request also
  // carries `X-PZ-Test: home page loads over the tunnel` so the daemon can
  // attribute exercised routes to this test (see `portzero inspect`).
  await page.goto('/');
  await expect(page).toHaveTitle(/.+/);
});
