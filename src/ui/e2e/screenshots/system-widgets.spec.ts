import type {
  BarLayoutItem,
  BarSnapshotOutcome_Serialize,
  BarWidgetKind,
} from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

import { skipWithoutMapleMono } from './font';

/*
 * Baselines of the default end zone with the five system widgets (overview
 * decision 9), from production `index.html` with the fake host and a fixed
 * clock. Compared only with 'Maple Mono NF' installed. Update with
 * `bunx playwright test --update-snapshots src/ui/e2e/screenshots/system-widgets.spec.ts`.
 */

const OPTIONS: Partial<Record<BarWidgetKind, object>> = {
  clock: { show_date: true, show_seconds: true, hour_cycle: 'h23' },
  cpu: { temperature_alert: 85 },
};

const END: BarWidgetKind[] = ['cpu', 'battery', 'keep_awake', 'wifi', 'clock'];

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
    layout: {
      start: [],
      center: [],
      end: END.map(
        (kind) => ({ id: kind, widget: kind, options: { ...OPTIONS[kind] } }) as BarLayoutItem,
      ),
    },
    detail: null,
    cpu: { usage: 23, temperature: 61 },
    battery: { percentage: 64, state: 'Discharging' },
    wifi: { status: 'Connected', networkName: 'Home', signalStrength: -58 },
    media: null,
    weatherEpoch: '0',
    keepAwake: { active: true, paused: false, enabled: true },
    tiling: null,
  },
};

// Wider than the compact breakpoint (2036px), so labels are the full ones.
test.use({ viewport: { width: 2400, height: 120 }, deviceScaleFactor: 2 });

for (const scheme of ['dark', 'light'] as const) {
  test(`system widgets end zone (${scheme})`, async ({ page, host }) => {
    // Only `Date` is fixed; timers keep running.
    await page.clock.setFixedTime(new Date(2026, 10, 11, 13, 4, 5));
    host.answerBarSnapshot(SNAPSHOT);
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto('/index.html?surface=bar');
    await skipWithoutMapleMono(page);

    const end = page.locator('[data-zone="end"]');
    const buttons = end.getByRole('button');

    await expect(buttons).toHaveCount(5);
    await expect(buttons.nth(4)).toHaveText('Wed Nov 11 13:04:05');

    for (let index = 0; index < 5; index += 1) {
      await expect(buttons.nth(index)).toHaveCSS('opacity', '1');
    }

    await page.evaluate(() => document.fonts.ready.then(() => undefined));
    await expect(end).toHaveScreenshot(`system-widgets-end-${scheme}.png`);
  });
}
