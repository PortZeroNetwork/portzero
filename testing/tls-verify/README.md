# TLS trust verification (task-66)

A minimal Playwright check answering one question: on a GitHub Actions
`ubuntu-latest` runner, after `portzero trust install`, do Chromium, Firefox,
and WebKit — **as downloaded by Playwright**, not as system-installed
browsers — trust the PortZero local CA over a real `https://*.portzero.local`
tunnel?

Run by [`.github/workflows/playwright-tls-verify.yml`](../../.github/workflows/playwright-tls-verify.yml),
which builds `portzero` from source, installs the CA, opens an HTTPS tunnel to
a trivial static page in [`www/`](www/), then runs this suite against it.

## Why this exists

Playwright downloads its own browser builds rather than using the system
Chrome/Firefox/Safari — so the question of whether the platform trust-store
work in `client/crates/daemon/src/tls/trust.rs` (system CA bundle + NSS
databases) actually reaches those builds isn't obvious from reading the code
alone. This verifies it empirically instead of assuming.

## Result (see `docs/known-limitations.md` for the up-to-date version)

All three engines currently need `ignoreHTTPSErrors: true` on this runner —
none trust the PortZero local CA out of the box yet, contrary to the original
expectation for Chromium/WebKit below. See `docs/known-limitations.md` for
what's understood about why and what a real fix would involve.

- **Chromium**: *expected* trusted (reads the shared `~/.pki/nssdb` NSS
  store, which `portzero trust install` seeds proactively) — but the first
  real CI run showed it untrusted here too, for an as-yet-unidentified
  reason.
- **WebKit**: *expected* trusted (WebKitGTK on Linux verifies against the
  system trust store via GnuTLS + p11-kit, which `portzero trust install`
  also updates) — but untrusted in practice, plausibly because `trust
  extract-compat` fails on this runner image (see `docs/known-limitations.md`).
- **Firefox**: expected **not** trusted by default. Playwright launches its
  bundled Firefox against a fresh, ephemeral profile per run; NSS databases
  are per-profile, and `portzero trust install` only seeds *existing*
  profiles under `~/.mozilla/firefox/*`.

These were originally documented expectations based on reading `trust.rs`;
the actual CI run is the source of truth, and for Chromium/WebKit it
disagreed with the theory — see the workflow's run history for the current
result.

## Running locally

Requires a Linux host with `portzero` built and its CA installed
(`portzero trust generate && sudo portzero trust install`), the daemon
running, and a tunnel already up on `:443`:

```sh
npm install
npx playwright install --with-deps chromium firefox webkit
PZ_TUNNEL_URL="https://your-tunnel.portzero.local" npx playwright test
```
