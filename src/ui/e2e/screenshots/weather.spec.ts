import type {
  BarSnapshotOutcome_Serialize,
  BarWeatherOutcome_Serialize,
} from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

import { skipWithoutMapleMono } from './font';

/*
 * Baselines of the Weather item, ready and loading, from production
 * `index.html` with the fake host; provider HTTP is answered by a Playwright
 * route and every other remote request is aborted. Compared only with
 * 'Maple Mono NF' installed. Update with
 * `bunx playwright test --update-snapshots src/ui/e2e/screenshots/weather.spec.ts`.
 */

const SNAPSHOT: BarSnapshotOutcome_Serialize = {
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
    layout: { start: [], center: [], end: [{ id: 'weather', widget: 'weather', options: {} }] },
    detail: null,
    cpu: null,
    battery: null,
    wifi: null,
    media: null,
    weatherEpoch: '0',
    keepAwake: null,
    tiling: null,
  },
};

const READY: BarWeatherOutcome_Serialize = {
  kind: 'ready',
  weather: {
    configRevision: '1',
    provider: 'openMeteo',
    apiKey: null,
    location: { source: 'defaultLocation', query: '41.15,-8.61' },
    refreshIntervalMinutes: 20,
  },
};

test.use({ viewport: { width: 2400, height: 120 }, deviceScaleFactor: 2 });

for (const state of ['ready', 'loading'] as const) {
  for (const scheme of ['dark', 'light'] as const) {
    test(`weather ${state} (${scheme})`, async ({ page, host }) => {
      await page.route(
        (url) => url.hostname !== 'localhost' && url.hostname !== '127.0.0.1',
        (route) => route.abort(),
      );
      await page.route('https://api.open-meteo.com/**', (route) =>
        state === 'ready'
          ? route.fulfill({
              contentType: 'application/json',
              body: JSON.stringify({
                current: { apparent_temperature: 17.2, weather_code: 2, is_day: 1 },
              }),
            })
          : route.fulfill({ status: 503, body: '' }),
      );
      host.answerBarSnapshot(SNAPSHOT);
      host.answer('bar_weather', READY);
      await page.emulateMedia({ colorScheme: scheme });
      await page.goto('/index.html?surface=bar');
      await skipWithoutMapleMono(page);

      const end = page.locator('[data-zone="end"]');
      const button = end.getByRole('button');

      await expect(button).toHaveText(state === 'ready' ? '18°C' : 'Loading...');
      await expect(button).toHaveCSS('opacity', '1');
      await page.evaluate(() => document.fonts.ready.then(() => undefined));
      await expect(end).toHaveScreenshot(`weather-${state}-${scheme}.png`);
    });
  }
}
