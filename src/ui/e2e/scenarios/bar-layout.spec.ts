import type { Page } from '@playwright/test';

import type {
  BarLayoutItem,
  BarLayoutView,
  BarSnapshotOutcome_Serialize,
  BarWidgetKind,
} from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

/* Driven only through the fake host, as the native host would. */

/* The options the host publishes for an entry that sets none. */
const DEFAULT_OPTIONS = {
  clock: { show_date: true, show_seconds: true, hour_cycle: 'h23' },
  cpu: { temperature_alert: 85 },
} as const;

const layoutItem = (id: string, widget: BarWidgetKind, options: object = {}) =>
  ({
    id,
    widget,
    options: { ...(DEFAULT_OPTIONS as Partial<Record<BarWidgetKind, object>>)[widget], ...options },
  }) as BarLayoutItem;

type Entry = string | [id: string, widget: BarWidgetKind];

const zoneOf = (entries: Entry[]) =>
  entries.map((entry) =>
    typeof entry === 'string'
      ? layoutItem(entry, entry as BarWidgetKind)
      : layoutItem(entry[0], entry[1]),
  );

const layoutOf = (start: Entry[], center: Entry[], end: Entry[]): BarLayoutView => ({
  start: zoneOf(start),
  center: zoneOf(center),
  end: zoneOf(end),
});

/* No items: every kind is registered now (widgets have their own specs). */
const EMPTY_LAYOUT = layoutOf([], [], []);

function available(sequence: string, layout: BarLayoutView): BarSnapshotOutcome_Serialize {
  return {
    kind: 'available',
    view: {
      generation: '1',
      sequence,
      configRevision: '1',
      panel: 'loaded',
      menu: { state: 'hidden', reason: null },
      frame: null,
      height: 32,
      padding: 8,
      layout,
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
}

const changed = (sequence: string, outcome: BarSnapshotOutcome_Serialize) => ({
  generation: '1',
  sequence,
  resnapshot: false,
  outcome,
});

const HARNESS = '/e2e/harness.html?surface=bar';

const items = (page: Page, zone: string) =>
  page
    .locator(`[data-zone="${zone}"] [data-item]`)
    .evaluateAll((slots) => slots.map((slot) => (slot as HTMLElement).dataset.item));

async function box(page: Page, selector: string) {
  const found = await page.locator(selector).boundingBox();

  if (!found) {
    throw new Error(`no box for ${selector}`);
  }

  return found;
}

test.describe('bar layout', () => {
  test('boots with fixture widgets in configured order and positions', async ({
    page,
    host,
    consoleErrors,
  }) => {
    host.answerBarSnapshot(
      available('1', layoutOf([['wide', 'spaces'], 'clock'], ['media'], ['cpu', 'weather'])),
    );
    await page.goto(HARNESS);

    await expect(page.getByTestId('widget-weather')).toBeVisible();
    expect(await items(page, 'start')).toEqual(['wide', 'clock']);
    expect(await items(page, 'center')).toEqual(['media']);
    expect(await items(page, 'end')).toEqual(['cpu', 'weather']);

    const root = await box(page, '[data-surface="bar"]');
    const wide = await box(page, '[data-testid="widget-wide"]');
    const media = await box(page, '[data-testid="widget-media"]');
    const weather = await box(page, '[data-testid="widget-weather"]');

    expect(wide.x).toBeLessThan(root.x + 2);
    expect(Math.abs(weather.x + weather.width - (root.x + root.width))).toBeLessThan(2);
    expect(Math.abs(media.x + media.width / 2 - (root.x + root.width / 2))).toBeLessThan(1);
    expect(host.commands()).toEqual(['bar_snapshot']);
    expect(consoleErrors).toEqual([]);
  });

  test('follows reorders, removals and additions while a stateful widget keeps its state', async ({
    page,
    host,
  }) => {
    host.answerBarSnapshot(available('1', layoutOf(['clock'], ['media'], ['cpu'])));
    await page.goto(HARNESS);

    const clock = page.getByTestId('widget-clock');

    await clock.click();
    await clock.click();
    await expect(clock).toHaveText('clock:2');

    await host.emitBarChanged(
      changed('2', available('2', layoutOf([], ['media'], ['cpu', 'clock']))),
    );
    await expect.poll(() => items(page, 'end')).toEqual(['cpu', 'clock']);
    await expect(clock).toHaveText('clock:2');

    await host.emitBarChanged(
      changed('3', available('3', layoutOf(['clock'], [['clock_2', 'clock']], ['weather']))),
    );
    await expect.poll(() => items(page, 'start')).toEqual(['clock']);
    await expect.poll(() => items(page, 'center')).toEqual(['clock_2']);
    await expect.poll(() => items(page, 'end')).toEqual(['weather']);
    await expect(clock).toHaveText('clock:2');
    await expect(page.getByTestId('widget-clock_2')).toHaveText('clock_2:0');
    await expect(page.getByTestId('widget-cpu')).toHaveCount(0);
    await expect(page.getByTestId('widget-media')).toHaveCount(0);
  });

  test('delivers an options change to the same widget instance, keeping its state', async ({
    page,
    host,
    consoleErrors,
  }) => {
    host.answerBarSnapshot(available('1', layoutOf([], [], ['cpu', 'clock'])));
    await page.goto(HARNESS);

    const clock = page.getByTestId('widget-clock');
    const cpu = page.getByTestId('widget-cpu');

    await expect(clock).toHaveAttribute(
      'data-options',
      JSON.stringify({ show_date: true, show_seconds: true, hour_cycle: 'h23' }),
    );
    await expect(cpu).toHaveAttribute('data-options', JSON.stringify({ temperature_alert: 85 }));
    await clock.click();
    await expect(clock).toHaveText('clock:1');

    await host.emitBarChanged(
      changed(
        '2',
        available('2', {
          start: [],
          center: [],
          end: [
            layoutItem('cpu', 'cpu', { temperature_alert: 95 }),
            layoutItem('clock', 'clock', { show_seconds: false, hour_cycle: 'h12' }),
          ],
        }),
      ),
    );
    await expect(clock).toHaveAttribute(
      'data-options',
      JSON.stringify({ show_date: true, show_seconds: false, hour_cycle: 'h12' }),
    );
    await expect(cpu).toHaveAttribute('data-options', JSON.stringify({ temperature_alert: 95 }));
    await expect(clock).toHaveText('clock:1');
    expect(await items(page, 'end')).toEqual(['cpu', 'clock']);
    await expect(page.locator('[data-exiting]')).toHaveCount(0);
    expect(consoleErrors).toEqual([]);
  });

  test('ignores an older sequence after a newer one', async ({ page, host }) => {
    host.answerBarSnapshot(available('5', layoutOf(['clock'], [], [])));
    await page.goto(HARNESS);
    await expect(page.getByTestId('widget-clock')).toBeVisible();

    await host.emitBarChanged(changed('7', available('7', layoutOf([], ['media'], []))));
    await expect.poll(() => items(page, 'center')).toEqual(['media']);
    await expect(page.locator('[data-exiting]')).toHaveCount(0);

    // Record whether the stale layout's clock ever mounts, even briefly.
    await page.evaluate(() => {
      const seen = window as unknown as { staleClockSeen: boolean };

      seen.staleClockSeen = false;
      new MutationObserver(() => {
        if (document.querySelector('[data-testid="widget-clock"]')) {
          seen.staleClockSeen = true;
        }
      }).observe(document.body, { childList: true, subtree: true });
    });
    await host.emitBarChanged(changed('6', available('6', layoutOf(['cpu'], [], ['clock']))));
    // A newer, distinguishable change: once it shows, the stale one was handled.
    await host.emitBarChanged(changed('8', available('8', layoutOf([], ['media'], ['cpu']))));
    await expect.poll(() => items(page, 'end')).toEqual(['cpu']);
    expect(await items(page, 'start')).toEqual([]);
    expect(await items(page, 'center')).toEqual(['media']);
    expect(
      await page.evaluate(() => (window as unknown as { staleClockSeen: boolean }).staleClockSeen),
    ).toBe(false);
  });

  test('shows the items once an unavailable Bar becomes available', async ({
    page,
    host,
    consoleErrors,
  }) => {
    host.answerBarSnapshot({
      kind: 'unavailable',
      reason: 'starting',
      detail: null,
      generation: '1',
      sequence: '1',
    });
    await page.goto(HARNESS);
    await expect(page.locator('[data-zone]')).toHaveCount(3);
    await expect.poll(() => host.commands()).toEqual(['bar_snapshot']);
    await expect(page.locator('[data-item]')).toHaveCount(0);

    await host.emitBarChanged(
      changed('2', available('2', layoutOf(['clock'], ['media'], ['cpu']))),
    );
    await expect(page.getByTestId('widget-clock')).toBeVisible();
    await expect(page.getByTestId('widget-media')).toBeVisible();
    await expect(page.getByTestId('widget-cpu')).toBeVisible();
    await expect(page.getByRole('alert')).toHaveCount(0);
    expect(consoleErrors).toEqual([]);
  });

  /* Catppuccin Latte and Mocha `text`, from the published palette. */
  for (const [scheme, text] of [
    ['light', 'rgb(76, 79, 105)'],
    ['dark', 'rgb(205, 214, 244)'],
  ] as const) {
    test(`production index.html with an empty layout (${scheme}): three empty zones`, async ({
      page,
      host,
      consoleErrors,
    }) => {
      const requests: string[] = [];

      page.on('request', (request) => requests.push(new URL(request.url()).pathname));
      await page.emulateMedia({ colorScheme: scheme });
      host.answerBarSnapshot(available('1', EMPTY_LAYOUT));
      await page.goto('/index.html?surface=bar');

      const root = page.locator('[data-surface="bar"]');

      await expect(page.locator('[data-zone]')).toHaveCount(3);
      await expect.poll(() => host.commands()).toEqual(['bar_snapshot']);
      await expect(root).toHaveCSS('height', '32px');
      await expect(root).toHaveCSS('color', text);
      await expect(page.locator('[data-item]')).toHaveCount(0);
      await expect(page.getByRole('alert')).toHaveCount(0);

      // The engine's own modules load; no widget module (a chunk per widget) does.
      const engine = new Set([
        'index.ts',
        'types.ts',
        'registry.ts',
        'resolve.ts',
        'WidgetSlot/index.ts',
        'WidgetSlot/WidgetSlot.tsx',
      ]);
      const widgetModules = requests
        .filter((path) => path.startsWith('/src/windows/Bar/widgets/'))
        .map((path) => path.slice('/src/windows/Bar/widgets/'.length))
        .filter((path) => !engine.has(path));

      expect(widgetModules).toEqual([]);
      expect(requests.filter((path) => path.includes('/tests/'))).toEqual([]);
      expect(consoleErrors).toEqual([]);
    });
  }
});
