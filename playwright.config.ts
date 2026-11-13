import { defineConfig, devices } from '@playwright/test';

/**
 * End-to-end scenarios (`src/ui/e2e/scenarios`) and screenshot baselines
 * (`src/ui/e2e/screenshots`, images in `screenshots/__screenshots__/`) against the Vite dev server and
 * the scripted fake host. Port 1430, never 1420, so a running `tauri:dev` is
 * never reused or disturbed.
 */
const PORT = 1430;

export default defineConfig({
  testDir: './src/ui/e2e',
  testMatch: ['scenarios/**/*.spec.ts', 'screenshots/**/*.spec.ts'],
  snapshotPathTemplate: '{testDir}/screenshots/__screenshots__/{testFileName}/{arg}{ext}',
  expect: { toHaveScreenshot: { animations: 'disabled', caret: 'hide', scale: 'device' } },
  outputDir: './test-results/e2e',
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: 'retain-on-failure',
    // Independent of the machine's Reduce Motion; a spec may still emulate `reduce`.
    reducedMotion: 'no-preference',
  },
  projects: [{ name: 'webkit', use: { ...devices['Desktop Safari'] } }],
  webServer: {
    command: `bunx vite --port ${PORT} --strictPort`,
    url: `http://localhost:${PORT}`,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
