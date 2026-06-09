import { defineConfig, devices } from '@playwright/test';

const PORT = 8080;
const baseURL = `http://127.0.0.1:${PORT}`;

// Tests the example exactly as shipped: the frontend baked by `build.rs` and served
// *embedded* in the binary — no live-reload, no on-the-fly compilation — so runs are
// deterministic. `WEB_MODULES_EMBEDDED=1` forces embedded serving even in a debug
// build (which compiles fast in CI). Point Playwright's `webServer` at any
// web-modules binary the same way to test your own frontend.
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
    command: process.env.E2E_SERVER ?? 'cargo run -p lit-element',
    cwd: '../..',
    env: { WEB_MODULES_EMBEDDED: '1' },
    url: baseURL,
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
  },
});
