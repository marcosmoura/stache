import type { Page, Route } from '@playwright/test';

import type {
  BarLayoutItem,
  BarSnapshotOutcome_Serialize,
  BarWeatherOutcome_Serialize,
  BarWeather_Serialize,
} from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

/*
 * The Weather widget with the production registry, from `index.html`. The fake
 * host answers `bar_weather`; provider HTTP is answered by Playwright routes and
 * any other remote request is aborted and recorded, so nothing leaves the machine.
 */

const PRODUCTION = '/index.html?surface=bar';
const KEY = 'SENTINEL-E2E-KEY';

const PROVIDERS = {
  visualCrossing: 'https://weather.visualcrossing.com/**',
  openMeteo: 'https://api.open-meteo.com/**',
  geocoding: 'https://geocoding-api.open-meteo.com/**',
};

const DEFAULT_END = ['weather', 'cpu', 'battery', 'keep_awake', 'wifi', 'clock'];

function available(sequence: string, configRevision = '1'): BarSnapshotOutcome_Serialize {
  return {
    kind: 'available',
    view: {
      generation: '1',
      sequence,
      configRevision,
      panel: 'loaded',
      menu: { state: 'hidden', reason: null },
      frame: null,
      height: 32,
      padding: 8,
      layout: {
        start: [],
        center: [],
        end: DEFAULT_END.map(
          (kind) =>
            ({
              id: kind,
              widget: kind,
              options:
                kind === 'clock'
                  ? { show_date: true, show_seconds: true, hour_cycle: 'h23' }
                  : kind === 'cpu'
                    ? { temperature_alert: 85 }
                    : {},
            }) as BarLayoutItem,
        ),
      },
      detail: null,
      cpu: { usage: 23, temperature: 61 },
      battery: null,
      wifi: null,
      media: null,
      weatherEpoch: '0',
      keepAwake: null,
      tiling: null,
    },
  };
}

const ready = (overrides: Partial<BarWeather_Serialize> = {}): BarWeatherOutcome_Serialize => ({
  kind: 'ready',
  weather: {
    configRevision: '1',
    provider: 'auto',
    apiKey: KEY,
    location: { source: 'defaultLocation', query: 'Porto' },
    refreshIntervalMinutes: 20,
    ...overrides,
  },
});

type Traffic = { provider: URL[]; unrouted: string[] };

/** Routes the three provider hosts; any other non-local request is aborted and recorded. */
async function routeNetwork(
  page: Page,
  handlers: Partial<Record<keyof typeof PROVIDERS, (route: Route) => Promise<void>>>,
) {
  const traffic: Traffic = { provider: [], unrouted: [] };

  await page.route(
    (url) => url.hostname !== 'localhost' && url.hostname !== '127.0.0.1',
    async (route) => {
      traffic.unrouted.push(route.request().url());
      await route.abort();
    },
  );

  for (const [name, pattern] of Object.entries(PROVIDERS)) {
    const handler = handlers[name as keyof typeof PROVIDERS];

    await page.route(pattern, async (route) => {
      traffic.provider.push(new URL(route.request().url()));
      await (handler ? handler(route) : route.abort());
    });
  }

  return traffic;
}

const json = (body: unknown) => (route: Route) =>
  route.fulfill({ contentType: 'application/json', body: JSON.stringify(body) });

const weather = (page: Page) => page.locator('[data-zone="end"] [data-item="weather"] button');

test.describe('Weather', () => {
  test.use({ viewport: { width: 2400, height: 200 } });

  test('Visual Crossing: the default layout starts the end zone with the weather', async ({
    page,
    host,
    consoleErrors,
  }) => {
    const traffic = await routeNetwork(page, {
      visualCrossing: json({ currentConditions: { feelslike: 17.2, icon: 'partly-cloudy-day' } }),
    });

    host.answerBarSnapshot(available('1'));
    host.answer('bar_weather', ready());
    await page.goto(PRODUCTION);

    await expect(weather(page)).toHaveText('18°C');
    await expect(page.locator('[data-zone="end"] [data-item]').first()).toHaveAttribute(
      'data-item',
      'weather',
    );
    expect(traffic.provider).toHaveLength(1);
    expect(traffic.provider[0]!.pathname).toBe(
      '/VisualCrossingWebServices/rest/services/timeline/Porto/next5days',
    );
    expect(traffic.provider[0]!.searchParams.get('key')).toBe(KEY);
    expect(traffic.unrouted).toEqual([]);
    expect(await page.content()).not.toContain(KEY);
    expect(consoleErrors).toEqual([]);
  });

  test('Open Meteo geocodes a place name and reads the night icon', async ({ page, host }) => {
    const traffic = await routeNetwork(page, {
      geocoding: json({ results: [{ latitude: 41.15, longitude: -8.61 }] }),
      openMeteo: json({ current: { apparent_temperature: 9.4, weather_code: 0, is_day: 0 } }),
    });

    host.answerBarSnapshot(available('1'));
    host.answer('bar_weather', ready({ apiKey: null }));
    await page.goto(PRODUCTION);

    await expect(weather(page)).toHaveText('10°C');
    expect(traffic.provider.map((url) => url.host)).toEqual([
      'geocoding-api.open-meteo.com',
      'api.open-meteo.com',
    ]);
    expect(traffic.provider[0]!.searchParams.get('name')).toBe('Porto');
    expect(traffic.provider[1]!.searchParams.get('latitude')).toBe('41.15');
    expect(traffic.provider[1]!.searchParams.has('key')).toBe(false);
    expect(traffic.unrouted).toEqual([]);
  });

  test('a provider error shows Loading... with no secret logged', async ({ page, host }) => {
    const logs: string[] = [];

    page.on('console', (message) => logs.push(message.text()));

    const traffic = await routeNetwork(page, {
      visualCrossing: (route) => route.fulfill({ status: 500, body: `bad key ${KEY}` }),
    });

    host.answerBarSnapshot(available('1'));
    host.answer('bar_weather', ready());
    await page.goto(PRODUCTION);

    await expect(weather(page)).toHaveText('Loading...');
    expect(traffic.provider).toHaveLength(1);
    expect(traffic.unrouted).toEqual([]);
    expect(logs.join('\n')).not.toContain(KEY);
    expect(await page.content()).not.toContain(KEY);
  });

  test('a config reload that changes the provider refetches with the new one', async ({
    page,
    host,
  }) => {
    const traffic = await routeNetwork(page, {
      visualCrossing: json({ currentConditions: { feelslike: 17, icon: 'rain' } }),
      openMeteo: json({ current: { apparent_temperature: 4.2, weather_code: 3, is_day: 1 } }),
    });

    host.answerBarSnapshot(available('1'));
    host.answer('bar_weather', ready());
    await page.goto(PRODUCTION);
    await expect(weather(page)).toHaveText('17°C');

    host.answer(
      'bar_weather',
      ready({
        configRevision: '2',
        provider: 'openMeteo',
        location: { source: 'defaultLocation', query: '41.15,-8.61' },
      }),
    );
    await host.emitBarChanged({
      generation: '1',
      sequence: '2',
      resnapshot: false,
      outcome: available('2', '2'),
    });

    await expect(weather(page)).toHaveText('5°C');
    expect(traffic.provider.map((url) => url.host)).toEqual([
      'weather.visualcrossing.com',
      'api.open-meteo.com',
    ]);

    // visual_crossing without a key is unconfigured: the item hides.
    host.answer(
      'bar_weather',
      ready({ configRevision: '3', provider: 'visualCrossing', apiKey: null }),
    );
    await host.emitBarChanged({
      generation: '1',
      sequence: '3',
      resnapshot: false,
      outcome: available('3', '3'),
    });
    await expect(page.locator('[data-item="weather"] button')).toHaveCount(0);
    expect(traffic.provider).toHaveLength(2);
    expect(traffic.unrouted).toEqual([]);
  });

  test('the click sends bar_action("weather")', async ({ page, host, consoleErrors }) => {
    await routeNetwork(page, {
      visualCrossing: json({ currentConditions: { feelslike: 17, icon: 'rain' } }),
    });
    host.answerBarSnapshot(available('1'));
    host.answer('bar_weather', ready());
    host.answer('bar_action', { kind: 'completed' });
    await page.goto(PRODUCTION);
    await expect(weather(page)).toHaveText('17°C');
    await weather(page).click();

    await expect
      .poll(() =>
        host.calls.filter((call) => call.command === 'bar_action').map((call) => call.args),
      )
      .toEqual([{ action: 'weather' }]);
    expect(host.unanswered).toEqual([]);
    expect(consoleErrors).toEqual([]);
  });
});
