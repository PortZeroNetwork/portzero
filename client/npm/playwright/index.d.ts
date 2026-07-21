import type { test as baseTest, expect as baseExpect } from '@playwright/test';

/** Error thrown when the Port Zero CLI/daemon is unavailable or a tunnel is unknown. */
export declare class PortzeroError extends Error {
  constructor(message: string);
}

/** HTTP basic-auth credentials for an access-controlled cloud tunnel. */
export interface PortzeroCredentials {
  username: string;
  password: string;
}

/** Options for {@link waitForTunnel}. */
export interface WaitOptions {
  /** Also poll the tunnel's health path until it returns 2xx. */
  healthy?: boolean;
  /** Maximum seconds to wait (defaults to the CLI's own default). */
  timeoutSecs?: number;
}

/** Options for {@link webServerFor}. */
export interface WebServerOptions {
  /** A command to run before waiting (e.g. `docker compose up -d`). */
  command?: string;
  /** Wait for the health path (default: true). */
  healthy?: boolean;
  /** Wait timeout in seconds. */
  timeoutSecs?: number;
  /** Reuse an already-running server (default: `!process.env.CI`). */
  reuseExistingServer?: boolean;
}

/**
 * Resolve a tunnel domain to its base URL by asking the daemon
 * (via `portzero url`). Throws {@link PortzeroError} if the daemon is not
 * running or the tunnel is unknown.
 */
export declare function resolveBaseUrl(domain: string): Promise<string>;

/**
 * Block until a tunnel is up (and, with `healthy`, until its health path
 * returns 2xx). Delegates to `portzero wait`.
 */
export declare function waitForTunnel(domain: string, opts?: WaitOptions): Promise<void>;

/** Build a Playwright `webServer` fragment that gates the run on a tunnel being up. */
export declare function webServerFor(
  domain: string,
  opts?: WebServerOptions,
): {
  command: string;
  url: string | undefined;
  reuseExistingServer: boolean;
  timeout: number;
};

/** Read httpCredentials from `PZ_HTTP_USERNAME`/`PZ_HTTP_PASSWORD`, or `undefined`. */
export declare function credentialsFromEnv(): PortzeroCredentials | undefined;

/** Port Zero test fixture options, settable via `test.use({ ... })`. */
export interface PortzeroOptions {
  /** Tunnel domain to resolve `baseURL` from (or set `PORTZERO_TUNNEL`). */
  portzeroTunnel?: string;
  /** Also wait for the tunnel's health path before tests (default: true). */
  portzeroWaitHealthy?: boolean;
  /** Readiness wait timeout in seconds. */
  portzeroWaitTimeoutSecs?: number;
  /** Stamp `X-PZ-Test: <test title>` on requests (default: true). */
  portzeroTestHeader?: boolean;
}

/** Build an extended Playwright `test` with Port Zero fixtures from a base module. */
export declare function createTest(base: {
  test: typeof baseTest;
}): typeof baseTest;

/** Extended Playwright `test` with Port Zero fixtures and options. */
export declare const test: typeof baseTest;

/** Re-exported Playwright `expect`. */
export declare const expect: typeof baseExpect;
