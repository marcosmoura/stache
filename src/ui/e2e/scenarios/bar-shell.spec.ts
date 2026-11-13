import type { Page } from '@playwright/test';

import type {
  BarLayoutItem,
  BarSnapshotOutcome_Serialize,
  MenuBarState,
} from '../../src/ipc/bindings.gen';
import { expect, test } from '../fixtures';

/* The Bar shell end to end, through the fake host and the harness fixture registry. */

const HARNESS = '/e2e/harness.html?surface=bar';

const media: BarLayoutItem = { id: 'media', widget: 'media', options: {} };

function available(sequence: string, menu: MenuBarState): BarSnapshotOutcome_Serialize {
  return {
    kind: 'available',
    view: {
      generation: '1',
      sequence,
      configRevision: '1',
      panel: 'loaded',
      menu: { state: menu, reason: null },
      frame: null,
      height: 32,
      padding: 8,
      layout: { start: [], center: [media], end: [] },
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

/** The Bar's vertical offset and opacity. */
const placement = (page: Page) =>
  page.locator('[data-surface="bar"]').evaluate((element) => {
    const style = getComputedStyle(element);

    return { y: new DOMMatrix(style.transform).m42, opacity: Number(style.opacity) };
  });

test.describe('Bar shell', () => {
  test('slides away while the menu bar is visible and returns when it hides', async ({
    page,
    host,
    consoleErrors,
  }) => {
    host.answerBarSnapshot(available('1', 'hidden'));
    await page.goto(HARNESS);

    await expect(page.getByTestId('widget-media')).toBeVisible();
    await expect.poll(() => placement(page)).toEqual({ y: 0, opacity: 1 });

    await host.emitBarChanged(changed('2', available('2', 'visible')));
    await expect.poll(() => placement(page)).toEqual({ y: 32, opacity: 0 });

    await host.emitBarChanged(changed('3', available('3', 'hidden')));
    await expect.poll(() => placement(page)).toEqual({ y: 0, opacity: 1 });
    expect(consoleErrors).toEqual([]);
  });

  test('a click on a BarItem widget records exactly one command', async ({
    page,
    host,
    consoleErrors,
  }) => {
    host.answerBarSnapshot(available('1', 'hidden'));
    host.answer('bar_action', { kind: 'completed' });
    await page.goto(HARNESS);

    const item = page.getByTestId('widget-media').getByRole('button');

    await expect(item).toBeVisible();
    await item.click();
    await expect
      .poll(() => host.calls.filter((call) => call.command === 'bar_action'))
      .toEqual([{ command: 'bar_action', args: { action: 'media' } }]);
    expect(host.unanswered).toEqual([]);
    expect(consoleErrors).toEqual([]);
  });

  test('the context menu does not open and text cannot be selected', async ({ page, host }) => {
    host.answerBarSnapshot(available('1', 'hidden'));
    await page.goto(HARNESS);
    await expect(page.getByTestId('widget-media')).toBeVisible();

    const prevented = await page.getByTestId('widget-media').evaluate((element) => {
      const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });

      element.dispatchEvent(event);

      return event.defaultPrevented;
    });

    expect(prevented).toBe(true);

    await page.getByTestId('widget-media').dblclick();
    expect(await page.evaluate(() => window.getSelection()?.toString() ?? '')).toBe('');
  });
});
