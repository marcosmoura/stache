import type { Page } from '@playwright/test';

import type {
  BarSnapshotOutcome_Serialize,
  StripApp,
  TilingStripView_Serialize,
} from '../../src/ipc/bindings.gen';
import { USER_WORKSPACES } from '../../tests/workspaces';
import type { FakeHost } from '../fakeHost';
import { expect, test } from '../fixtures';

/*
 * The Spaces widget with the production registry, from `index.html`: Tiling
 * loads, publishes several workspaces and apps, focus moves to another app and
 * another workspace, windows open and close, an app gets a second window,
 * Tiling goes absent and returns, and clicks send the rendered counters (a
 * stale outcome changes nothing).
 */

const PRODUCTION = '/index.html?surface=bar';

const app = (key: string, appName: string, title = ''): StripApp => ({ key, appName, title });

const GHOSTTY = app('11', 'Ghostty', 'zsh');
const CODE = app('12', 'Code', 'stache — Spaces.tsx');
const SAFARI = app('21', 'Safari', 'Docs');
const SPOTIFY = app('31', 'Spotify', 'Spotify');

/* The user's workspaces in their Tiling configuration's order; the UI keeps it. */
const WORKSPACES = USER_WORKSPACES;

const strip = (
  generation: string,
  sequence: string,
  focusedWorkspace: string,
  apps: StripApp[],
  focusedApp: StripApp | null,
): TilingStripView_Serialize => ({
  kind: 'available',
  generation,
  sequence,
  workspaces: WORKSPACES.map((name) => ({ name, focused: name === focusedWorkspace })),
  apps,
  focusedApp,
  focusedWorkspace,
});

function available(
  sequence: string,
  tiling: TilingStripView_Serialize | null,
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
      layout: {
        start: [{ id: 'spaces', widget: 'spaces', options: {} }],
        center: [],
        end: [
          {
            id: 'clock',
            widget: 'clock',
            options: { show_date: true, show_seconds: true, hour_cycle: 'h23' },
          },
        ],
      },
      detail: null,
      cpu: null,
      battery: null,
      wifi: null,
      media: null,
      weatherEpoch: '0',
      keepAwake: null,
      tiling,
    },
  };
}

let sequence = 1;

const emit = (host: FakeHost, tiling: TilingStripView_Serialize | null) => {
  sequence += 1;

  return host.emitBarChanged({
    generation: '1',
    sequence: String(sequence),
    resnapshot: false,
    outcome: available(String(sequence), tiling),
  });
};

const start = (page: Page) => page.locator('[data-zone="start"]');
const workspaceNames = (page: Page) =>
  start(page)
    .getByRole('group', { name: 'Workspaces' })
    .getByRole('button')
    .evaluateAll((buttons) => buttons.map((button) => button.getAttribute('aria-label')));
const appKeys = (page: Page) =>
  start(page)
    .locator('[data-app]')
    .evaluateAll((items) => items.map((item) => (item as HTMLElement).dataset.app));
/* The focused app is the one item without an `aria-label`: its visible label names it. */
const focusedLabel = (page: Page) => start(page).locator('[data-app] > button:not([aria-label])');
const pressed = (page: Page) =>
  start(page).locator('button[aria-pressed="true"]').getAttribute('aria-label');

test.describe('Spaces', () => {
  test.use({ viewport: { width: 2400, height: 200 } });

  test.beforeEach(() => {
    sequence = 1;
  });

  test('a realistic session: focus, windows, absent and back, clicks', async ({
    page,
    host,
    consoleErrors,
  }) => {
    host.answer('bar_tiling_action', { kind: 'applied' });
    host.answerBarSnapshot(available('1', { kind: 'loading' }));
    await page.goto(PRODUCTION);

    // Loading.
    await expect(start(page)).toHaveText('Loading...');

    // Tiling publishes: the workspaces in published order, terminal focused with two apps.
    await emit(host, strip('5', '10', 'terminal', [GHOSTTY, CODE], GHOSTTY));
    await expect.poll(() => workspaceNames(page)).toEqual(USER_WORKSPACES);
    await expect.poll(() => appKeys(page)).toEqual(['11', '12']);
    await expect(focusedLabel(page)).toHaveText('Ghostty');
    expect(await pressed(page)).toBe('terminal');

    // Every item fills the Bar height.
    const heights = await start(page)
      .locator('[role="group"], [data-app] > button')
      .evaluateAll((items) => items.map((item) => item.getBoundingClientRect().height));

    expect(heights).toEqual([32, 32, 32]);

    // Focus another app.
    await emit(host, strip('5', '11', 'terminal', [GHOSTTY, CODE], CODE));
    await expect(focusedLabel(page)).toHaveText('Code');

    // Open a second Code window: both Code windows show their titles.
    const CODE_2 = app('13', 'Code', 'notes.md');

    await emit(host, strip('5', '12', 'terminal', [GHOSTTY, CODE, CODE_2], CODE_2));
    await expect.poll(() => appKeys(page)).toEqual(['11', '12', '13']);
    await expect(focusedLabel(page)).toHaveText('notes.md');
    await expect(start(page).getByRole('button', { name: 'stache — Spaces.tsx' })).toBeVisible();

    // Close it again.
    await emit(host, strip('5', '13', 'terminal', [GHOSTTY, CODE], CODE));
    await expect.poll(() => appKeys(page)).toEqual(['11', '12']);
    await expect(focusedLabel(page)).toHaveText('Code');

    // Focus another workspace: its windows replace the list.
    await emit(host, strip('5', '14', 'browser', [SAFARI], SAFARI));
    await expect.poll(() => appKeys(page)).toEqual(['21']);
    await expect(focusedLabel(page)).toHaveText('Safari');
    await expect.poll(() => pressed(page)).toBe('browser');

    // Click a workspace and an app: the rendered generation and sequence go along.
    await start(page).getByRole('button', { name: 'misc' }).click();
    await expect
      .poll(() =>
        host.calls.filter((call) => call.command === 'bar_tiling_action').map((call) => call.args),
      )
      .toEqual([
        {
          request: {
            generation: '5',
            sequence: '14',
            action: { kind: 'focusWorkspace', name: 'misc' },
          },
        },
      ]);
    await start(page).locator('[data-app="21"] button').click();
    await expect
      .poll(() => host.calls.filter((call) => call.command === 'bar_tiling_action').length)
      .toBe(2);
    expect(host.calls.filter((call) => call.command === 'bar_tiling_action')[1]!.args).toEqual({
      request: { generation: '5', sequence: '14', action: { kind: 'focusWindow', window: '21' } },
    });

    // A stale outcome draws nothing and changes nothing.
    host.answer('bar_tiling_action', { kind: 'stale' });
    await start(page).getByRole('button', { name: 'tasks' }).click();
    await expect
      .poll(() => host.calls.filter((call) => call.command === 'bar_tiling_action').length)
      .toBe(3);
    expect(await pressed(page)).toBe('browser');
    await expect(focusedLabel(page)).toHaveText('Safari');

    // Tiling goes absent: the strip disappears.
    await emit(host, { kind: 'absent' });
    await expect(start(page).locator('[data-spaces]')).toHaveCount(0);
    await expect(start(page)).toHaveText('');

    // It returns with a new generation; a click carries it.
    await emit(host, strip('6', '1', 'media', [SPOTIFY], SPOTIFY));
    await expect(focusedLabel(page)).toHaveText('Spotify');
    host.answer('bar_tiling_action', { kind: 'applied' });
    await start(page).getByRole('button', { name: 'terminal' }).click();
    await expect
      .poll(() => host.calls.filter((call) => call.command === 'bar_tiling_action').length)
      .toBe(4);
    expect(host.calls.filter((call) => call.command === 'bar_tiling_action')[3]!.args).toEqual({
      request: {
        generation: '6',
        sequence: '1',
        action: { kind: 'focusWorkspace', name: 'terminal' },
      },
    });
    expect(consoleErrors).toEqual([]);
  });
});
