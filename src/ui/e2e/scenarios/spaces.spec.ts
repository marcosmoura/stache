import type { Page } from '@playwright/test';

import type {
  BarSnapshotOutcome_Serialize,
  StripApp,
  StripRefusedFocus,
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
  workspaces: WORKSPACES.map((name) => ({
    name,
    focused: name === focusedWorkspace,
    inactive: false,
  })),
  apps,
  focusedApp,
  focusedWorkspace,
  refusedFocus: null,
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

  test('inactive workspaces: dimmed, refused clicks flash, a reload does not replay, then active', async ({
    page,
    host,
    consoleErrors,
  }) => {
    // Counts every refusal flash that starts, per workspace, from the first script on each load.
    await page.addInitScript(() => {
      const flashes: string[] = [];

      (window as unknown as { __flashes: string[] }).__flashes = flashes;
      new MutationObserver((records) => {
        for (const record of records) {
          for (const node of record.addedNodes) {
            if (node instanceof HTMLElement && node.matches('[data-flash]')) {
              flashes.push(node.parentElement?.getAttribute('aria-label') ?? '');
            }
          }
        }
      }).observe(document, { childList: true, subtree: true });
    });
    const flashes = () =>
      page.evaluate(() => [...(window as unknown as { __flashes: string[] }).__flashes]);
    const actions = () =>
      host.calls.filter((call) => call.command === 'bar_tiling_action').map((call) => call.args);
    const tasks = () => start(page).getByRole('button', { name: 'tasks' });

    // The user's workspaces: files holds a Finder window, terminal is focused,
    // tasks and misc are empty and cannot be shown as empty desktops.
    const withInactive = (
      sequence: string,
      inactive: string[],
      refusedFocus: StripRefusedFocus | null,
    ): Extract<TilingStripView_Serialize, { kind: 'available' }> => ({
      ...(strip('7', sequence, 'terminal', [GHOSTTY], GHOSTTY) as Extract<
        TilingStripView_Serialize,
        { kind: 'available' }
      >),
      workspaces: WORKSPACES.map((name) => ({
        name,
        focused: name === 'terminal',
        inactive: inactive.includes(name),
      })),
      refusedFocus,
    });

    host.answer('bar_tiling_action', { kind: 'refused' });
    host.answerBarSnapshot(available('1', withInactive('1', ['tasks', 'misc'], null)));
    await page.goto(PRODUCTION);

    await expect.poll(() => workspaceNames(page)).toEqual(USER_WORKSPACES);

    for (const name of ['tasks', 'misc']) {
      const item = start(page).getByRole('button', { name });

      await expect(item).toHaveAttribute('aria-disabled', 'true');
      await expect(item).toHaveAttribute('data-inactive', 'true');
      await expect(item.locator('svg')).toHaveCSS('opacity', '0.4');
    }

    await expect(start(page).locator('[data-inactive]')).toHaveCount(2);

    // Click tasks: the request goes to Tiling, which refuses and publishes the refusal.
    // (`force`: Playwright will not click an `aria-disabled` element; a user can.)
    await tasks().click({ force: true });
    await expect.poll(actions).toEqual([
      {
        request: {
          generation: '7',
          sequence: '1',
          action: { kind: 'focusWorkspace', name: 'tasks' },
        },
      },
    ]);
    await emit(host, withInactive('2', ['tasks', 'misc'], { workspace: 'tasks', serial: 1 }));
    await expect.poll(flashes).toEqual(['tasks']);
    await expect(tasks()).not.toHaveAttribute('data-flashing');
    expect(await pressed(page)).toBe('terminal');

    // A second refusal flashes again.
    await tasks().click({ force: true });
    await expect.poll(() => actions().length).toBe(2);
    await emit(host, withInactive('3', ['tasks', 'misc'], { workspace: 'tasks', serial: 2 }));
    await expect.poll(flashes).toEqual(['tasks', 'tasks']);
    await expect(tasks()).not.toHaveAttribute('data-flashing');

    // A fresh load carrying the same refusal does not replay it, nor does a later strip.
    host.answerBarSnapshot(
      available('1', withInactive('3', ['tasks', 'misc'], { workspace: 'tasks', serial: 2 })),
    );
    await page.reload();
    await expect(tasks()).toHaveAttribute('data-inactive', 'true');
    await emit(host, withInactive('4', ['tasks', 'misc'], { workspace: 'tasks', serial: 2 }));
    await expect
      .poll(() => page.evaluate(() => document.querySelectorAll('[data-inactive]').length))
      .toBe(2);
    expect(await flashes()).toEqual([]);

    // A window moves to tasks: it becomes active and a click focuses it.
    await emit(host, withInactive('5', ['misc'], { workspace: 'tasks', serial: 2 }));
    await expect(tasks()).not.toHaveAttribute('aria-disabled');
    await expect(tasks()).not.toHaveAttribute('data-inactive');
    await expect(tasks().locator('svg')).toHaveCSS('opacity', '1');
    host.answer('bar_tiling_action', { kind: 'applied' });
    await tasks().click();
    await expect.poll(() => actions().length).toBe(3);
    expect(actions()[2]).toEqual({
      request: {
        generation: '7',
        sequence: '5',
        action: { kind: 'focusWorkspace', name: 'tasks' },
      },
    });
    await emit(host, {
      ...withInactive('6', ['misc'], { workspace: 'tasks', serial: 2 }),
      focusedWorkspace: 'tasks',
      workspaces: WORKSPACES.map((name) => ({
        name,
        focused: name === 'tasks',
        inactive: name === 'misc',
      })),
      apps: [],
      focusedApp: null,
    });
    await expect.poll(() => pressed(page)).toBe('tasks');
    expect(await flashes()).toEqual([]);
    expect(consoleErrors).toEqual([]);
    expect(host.unanswered).toEqual([]);
  });
});
