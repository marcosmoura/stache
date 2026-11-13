import type {
  BarSnapshotOutcome_Serialize,
  StripApp,
  TilingStripView_Serialize,
} from '../../src/ipc/bindings.gen';
import { USER_WORKSPACES } from '../../tests/workspaces';
import { expect, test } from '../fixtures';

import { skipWithoutMapleMono } from './font';

/*
 * Baselines of the Spaces strip (wide: every app, compact: the focused app
 * only) from production `index.html` with the fake host. Compared only with
 * 'Maple Mono NF' installed. Update with
 * `bunx playwright test src/ui/e2e/screenshots/spaces.spec.ts --update-snapshots`.
 */

const app = (key: string, appName: string, title: string): StripApp => ({ key, appName, title });

const APPS = [
  app('1', 'Ghostty', 'zsh'),
  app('2', 'Code', 'stache — Spaces.tsx'),
  app('3', 'Code', 'README.md'),
  app('4', 'Spotify', 'Spotify'),
];

const TILING: TilingStripView_Serialize = {
  kind: 'available',
  generation: '1',
  sequence: '1',
  workspaces: USER_WORKSPACES.map((name) => ({
    name,
    focused: name === 'coding',
    inactive: false,
  })),
  apps: APPS,
  focusedApp: APPS[1]!,
  focusedWorkspace: 'coding',
  refusedFocus: null,
};

const snapshot: BarSnapshotOutcome_Serialize = {
  kind: 'available',
  view: {
    generation: '1',
    sequence: '1',
    configRevision: '1',
    panel: 'loaded',
    menu: { state: 'hidden', reason: null },
    frame: null,
    height: 32,
    padding: 8,
    layout: { start: [{ id: 'spaces', widget: 'spaces', options: {} }], center: [], end: [] },
    detail: null,
    cpu: null,
    battery: null,
    wifi: null,
    media: null,
    weatherEpoch: '0',
    keepAwake: null,
    tiling: TILING,
  },
};

const WIDTHS = { wide: 2400, compact: 1440 } as const;

test.use({ deviceScaleFactor: 2 });

for (const [mode, width] of Object.entries(WIDTHS)) {
  for (const scheme of ['dark', 'light'] as const) {
    test(`spaces ${mode} (${scheme})`, async ({ page, host }) => {
      await page.setViewportSize({ width, height: 120 });
      host.answerBarSnapshot(snapshot);
      await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
      await page.goto('/index.html?surface=bar');
      await skipWithoutMapleMono(page);

      const strip = page.locator('[data-spaces]');
      const focused = strip.locator('[data-app="2"] > button');

      await expect(focused).toHaveText(mode === 'wide' ? 'stache — Spaces.tsx' : 'Code');
      await expect(strip.locator('[data-app]')).toHaveCount(mode === 'wide' ? 4 : 1);
      await expect(strip.locator('[data-highlight]')).toHaveCount(1);
      // Settled: every item has finished fading in, the label is fully revealed.
      await expect(focused).toHaveCSS('opacity', '1');
      await expect(strip.locator('[data-label]')).toHaveCSS('opacity', '1');
      await page.evaluate(() => document.fonts.ready.then(() => undefined));
      await expect(strip).toHaveScreenshot(`spaces-${mode}-${scheme}.png`);
    });
  }
}

/* The same strip with `tasks` and `misc` inactive (dimmed); `coding` stays focused. */
const INACTIVE: BarSnapshotOutcome_Serialize = {
  ...snapshot,
  view: {
    ...(snapshot as Extract<BarSnapshotOutcome_Serialize, { kind: 'available' }>).view,
    tiling: {
      ...TILING,
      workspaces: USER_WORKSPACES.map((name) => ({
        name,
        focused: name === 'coding',
        inactive: name === 'tasks' || name === 'misc',
      })),
    },
  },
};

for (const [mode, width] of Object.entries(WIDTHS)) {
  for (const scheme of ['dark', 'light'] as const) {
    test(`spaces inactive ${mode} (${scheme})`, async ({ page, host }) => {
      await page.setViewportSize({ width, height: 120 });
      host.answerBarSnapshot(INACTIVE);
      await page.emulateMedia({ colorScheme: scheme, reducedMotion: 'reduce' });
      await page.goto('/index.html?surface=bar');
      await skipWithoutMapleMono(page);

      const strip = page.locator('[data-spaces]');
      const focused = strip.locator('[data-app="2"] > button');

      await expect(strip.locator('[data-inactive]')).toHaveCount(2);
      await expect(strip.locator('[data-highlight]')).toHaveCount(1);
      await expect(focused).toHaveCSS('opacity', '1');
      await expect(strip.locator('[data-label]')).toHaveCSS('opacity', '1');
      await page.evaluate(() => document.fonts.ready.then(() => undefined));
      await expect(strip).toHaveScreenshot(`spaces-inactive-${mode}-${scheme}.png`);
    });
  }
}
