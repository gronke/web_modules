import { defineConfig, devices } from '@playwright/test';

const PORT = 8080;
const baseURL = `http://127.0.0.1:${PORT}`;

// Tests the example exactly as shipped: the rolldown bundle baked by `build.rs` and served
// *embedded* in the binary — no bundler at runtime, deterministic. The example is excluded
// from the workspace, so the server is launched via `--manifest-path` (run from this
// directory). The first build is slow (rolldown compiles + npm install over the network),
// hence the generous timeout; CI builds it in a prior step so the run is instant.
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
    command: process.env.E2E_SERVER ?? 'cargo run --manifest-path Cargo.toml',
    url: baseURL,
    reuseExistingServer: !process.env.CI,
    timeout: 300_000,
  },
});
