import { defineConfig, devices } from '@playwright/test';

const PORT = 8081;
const baseURL = `http://127.0.0.1:${PORT}`;

// Tests the example exactly as shipped: web-modules vendors the two UMD files and serves the
// web/ tree, transforming app.ts on the fly. The server is `cargo run -p react-umd` (a normal
// workspace member, run from the repo root). On first run it vendors over the network, hence
// the generous timeout; CI warms the cache in a prior step.
export default defineConfig({
  testDir: './tests',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['github'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL,
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    // CI runs the prebuilt binary ($E2E_SERVER, an absolute path from the build job's artifact);
    // locally it falls back to `cargo run`.
    command: process.env.E2E_SERVER ?? 'cargo run -p react-umd',
    cwd: '../..',
    url: baseURL,
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
  },
});
