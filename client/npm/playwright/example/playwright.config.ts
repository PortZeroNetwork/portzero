// Example Playwright config using @portzero/playwright.
//
// Local dev and CI use the SAME config. The tunnel domain is the app's only
// identity — no ports, no localhost, no environment-specific baseURL.
import { defineConfig } from '@playwright/test';
import { webServerFor } from '@portzero/playwright';

const TUNNEL = process.env.PORTZERO_TUNNEL || 'web.myapp.portzero.local';

export default defineConfig({
  testDir: './tests',

  // Bring the stack up and block until the tunnel is healthy before any test
  // runs. Works identically on a laptop and in GitHub Actions.
  webServer: webServerFor(TUNNEL, {
    command: 'docker compose up -d',
    healthy: true,
    timeoutSecs: 120,
  }),

  use: {
    // The fixtures resolve baseURL from the daemon per test; this is a sensible
    // fallback for tooling that reads the config statically.
    baseURL: `http://${TUNNEL}`,
  },
});
