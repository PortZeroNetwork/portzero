import { test, expect } from "@playwright/test";

// Minimal check (task-66): does this engine load the tunneled HTTPS page at
// all (real trust for chromium/webkit, the documented ignoreHTTPSErrors
// fallback for firefox — see playwright.config.ts), and does it actually
// receive the expected content through the tunnel?
test("loads the tunneled HTTPS page", async ({ page, browserName }) => {
  const response = await page.goto("/");
  expect(response, `${browserName}: navigation should succeed`).toBeTruthy();
  expect(response!.ok(), `${browserName}: expected a 2xx response`).toBeTruthy();
  await expect(page.locator("body")).toContainText("PortZero TLS check");

  // securityDetails() is only implemented for Chromium/WebKit in Playwright;
  // record what we can as an annotation rather than asserting on it, so the
  // report is informative without making the check engine-specific.
  const securityDetails = await response!.securityDetails();
  test.info().annotations.push({
    type: "tls-trust",
    description: securityDetails
      ? `${browserName}: issuer=${securityDetails.issuer}`
      : `${browserName}: no securityDetails() available from this engine`,
  });
});
