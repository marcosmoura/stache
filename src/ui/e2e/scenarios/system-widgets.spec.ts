import type { Page } from '@playwright/test';

import type {
  BarLayoutItem,
  BarLayoutView,
  BarSnapshotOutcome_Serialize,
  BarView_Serialize,
  BarWidgetKind,
} from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

/*
 * The system widgets (Clock, CPU, Battery, Keep Awake, Wi-Fi) with the
 * production registry, booted from `index.html` and driven only through the
 * fake host, as the native host would.
 */

const PRODUCTION = '/index.html?surface=bar';

const DEFAULT_OPTIONS: Partial<Record<BarWidgetKind, object>> = {
  clock: { show_date: true, show_seconds: true, hour_cycle: 'h23' },
  cpu: { temperature_alert: 85 },
};

const item = (id: string, widget: BarWidgetKind, options: object = {}) =>
  ({ id, widget, options: { ...DEFAULT_OPTIONS[widget], ...options } }) as BarLayoutItem;

/*
 * The default layout without Weather (weather.spec.ts covers it with its provider
 * routes), Spaces (spaces.spec.ts) and Media (media.spec.ts).
 */
const DEFAULT_LAYOUT: BarLayoutView = {
  start: [],
  center: [],
  end: ['cpu', 'battery', 'keep_awake', 'wifi', 'clock'].map((kind) =>
    item(kind, kind as BarWidgetKind),
  ),
};

const READINGS: Partial<BarView_Serialize> = {
  cpu: { usage: 23, temperature: 61 },
  battery: { percentage: 64, state: 'Discharging' },
  keepAwake: { active: true, paused: false, enabled: true },
  wifi: { status: 'Connected', networkName: 'Home', signalStrength: -58 },
};

function available(
  sequence: string,
  overrides: Partial<BarView_Serialize> = {},
): BarSnapshotOutcome_Serialize {
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
      layout: DEFAULT_LAYOUT,
      detail: null,
      cpu: null,
      battery: null,
      wifi: null,
      media: null,
      weatherEpoch: '0',
      keepAwake: null,
      tiling: null,
      ...overrides,
    },
  };
}

const changed = (sequence: string, outcome: BarSnapshotOutcome_Serialize) => ({
  generation: '1',
  sequence,
  resnapshot: false,
  outcome,
});

/** The widget modules the page requested (paths under `/src/windows/Bar/widgets/<Name>/`). */
function trackWidgetChunks(page: Page) {
  const loaded = new Set<string>();

  page.on('request', (request) => {
    const match = /\/src\/windows\/Bar\/widgets\/([A-Z]\w+)\//.exec(
      new URL(request.url()).pathname,
    );

    if (match && !['WidgetSlot'].includes(match[1]!)) {
      loaded.add(match[1]!);
    }
  });

  return loaded;
}

const end = (page: Page) => page.locator('[data-zone="end"]');
const itemsOf = (page: Page, zone: string) =>
  page
    .locator(`[data-zone="${zone}"] [data-item]`)
    .evaluateAll((slots) => slots.map((slot) => (slot as HTMLElement).dataset.item));
const clockText = (page: Page, zone = 'end') =>
  page.locator(`[data-zone="${zone}"] [data-item="clock"] button`).textContent();

test.describe('System widgets', () => {
  // Wider than the compact breakpoint (2036px), so labels are the full ones.
  test.use({ viewport: { width: 2400, height: 200 } });

  test('the default layout (without Weather) shows CPU, Battery, Keep Awake, Wi-Fi and Clock in order', async ({
    page,
    host,
    consoleErrors,
  }) => {
    const chunks = trackWidgetChunks(page);

    host.answerBarSnapshot(available('1', READINGS));
    await page.goto(PRODUCTION);

    const buttons = end(page).getByRole('button');

    await expect(buttons).toHaveCount(5);
    await expect(buttons.nth(0)).toHaveText('23%61°C');
    await expect(buttons.nth(1)).toHaveText('64% (Discharging)');
    await expect(buttons.nth(2)).toHaveAccessibleName('Keep Awake: on');
    await expect(buttons.nth(3)).toHaveText('Home');
    await expect(buttons.nth(4)).toHaveText(/^\w{3} \w{3} \d{2} \d{2}:\d{2}:\d{2}$/);
    expect(await itemsOf(page, 'end')).toEqual(['cpu', 'battery', 'keep_awake', 'wifi', 'clock']);

    // Every item fills the Bar's height (32 here), whatever its content.
    const heights = await buttons.evaluateAll((items) =>
      items.map((item) => item.getBoundingClientRect().height),
    );

    expect(heights).toEqual([32, 32, 32, 32, 32]);
    await expect(page.locator('[data-zone="start"] [data-item]')).toHaveCount(0);
    expect([...chunks].sort()).toEqual(['Battery', 'Clock', 'Cpu', 'KeepAwake', 'Wifi']);

    // Compact mode (width <= 2036px) drops the battery state.
    await page.setViewportSize({ width: 2036, height: 200 });
    await expect(buttons.nth(1)).toHaveText('64%');
    expect(consoleErrors).toEqual([]);
  });

  test('a widget chunk loads only when its kind is in the layout', async ({ page, host }) => {
    const chunks = trackWidgetChunks(page);

    host.answerBarSnapshot(
      available('1', {
        ...READINGS,
        layout: { start: [], center: [], end: [item('clock', 'clock'), item('cpu', 'cpu')] },
      }),
    );
    await page.goto(PRODUCTION);
    await expect(end(page).getByRole('button')).toHaveCount(2);
    expect([...chunks].sort()).toEqual(['Clock', 'Cpu']);

    await host.emitBarChanged(changed('2', available('2', READINGS)));
    await expect(end(page).getByRole('button')).toHaveCount(5);
    expect([...chunks].sort()).toEqual(['Battery', 'Clock', 'Cpu', 'KeepAwake', 'Wifi']);
  });

  test('readings change live and each click sends exactly its command', async ({
    page,
    host,
    consoleErrors,
  }) => {
    host.answerBarSnapshot(available('1', READINGS));
    host.answer('bar_action', { kind: 'completed' });
    host.answer('bar_session_action', { kind: 'applied' });
    await page.emulateMedia({ colorScheme: 'dark' });
    await page.goto(PRODUCTION);

    const buttons = end(page).getByRole('button');

    await expect(buttons).toHaveCount(5);

    await host.emitBarChanged(
      changed(
        '2',
        available('2', {
          ...READINGS,
          cpu: { usage: 97, temperature: 91 },
          battery: { percentage: 64, state: 'Charging' },
          wifi: { status: 'Off', networkName: null, signalStrength: null },
          keepAwake: { active: false, paused: true, enabled: true },
        }),
      ),
    );
    await expect(buttons.nth(0)).toHaveText('97%91°C');
    await expect(buttons.nth(0).locator('svg')).toHaveCSS('color', 'rgb(243, 139, 168)');
    await expect(buttons.nth(1)).toHaveText('64% (Charging)');
    await expect(buttons.nth(1).locator('svg')).toHaveCSS('color', 'rgb(166, 227, 161)');
    await expect(buttons.nth(2)).toHaveAccessibleName('Keep Awake: off');
    await expect(buttons.nth(3)).toHaveText('Wi-Fi Off');
    await expect(buttons.nth(3).locator('svg')).toHaveCSS('color', 'rgb(243, 139, 168)');

    for (let index = 0; index < 5; index += 1) {
      await buttons.nth(index).click();
    }

    await expect
      .poll(() =>
        host.calls
          .filter((call) => ['bar_action', 'bar_session_action'].includes(call.command))
          .map((call) => call.args),
      )
      .toEqual([
        { action: 'cpu' },
        { action: 'battery' },
        { module: 'keepAwake', action: 'resume' },
        { action: 'wifi' },
        { action: 'clock' },
      ]);
    expect(host.unanswered).toEqual([]);
    expect(consoleErrors).toEqual([]);
  });

  test('a reload with a Clock option change and a reordered layout keeps the Clock ticking', async ({
    page,
    host,
    consoleErrors,
  }) => {
    host.answerBarSnapshot(available('1', READINGS));
    await page.goto(PRODUCTION);
    await expect(end(page).getByRole('button')).toHaveCount(5);

    const first = await clockText(page);

    await expect.poll(() => clockText(page), { timeout: 3000 }).not.toBe(first);

    await host.emitBarChanged(
      changed(
        '2',
        available('2', {
          ...READINGS,
          layout: {
            start: [item('clock', 'clock', { show_date: false })],
            center: [],
            end: [item('wifi', 'wifi'), item('cpu', 'cpu')],
          },
        }),
      ),
    );

    await expect.poll(() => itemsOf(page, 'start')).toEqual(['clock']);
    expect(await itemsOf(page, 'end')).toEqual(['wifi', 'cpu']);
    await expect(page.locator('[data-zone="start"] button')).toHaveText(/^\d{2}:\d{2}:\d{2}$/);

    const moved = await clockText(page, 'start');

    await expect.poll(() => clockText(page, 'start'), { timeout: 3000 }).not.toBe(moved);
    expect(consoleErrors).toEqual([]);
  });
});
