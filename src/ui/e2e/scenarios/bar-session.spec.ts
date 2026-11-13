import type { Page } from '@playwright/test';

import type {
  BarLayoutItem,
  BarLayoutView,
  BarSnapshotOutcome_Serialize,
  BarView_Serialize,
  MediaReading_Serialize,
  StripApp,
  TilingStripView_Serialize,
} from '../../src/ipc/bindings.gen';
import { WARM_PNG } from '../../tests/png';
import { USER_WORKSPACES } from '../../tests/workspaces';
import type { FakeHost } from '../fakeHost';
import { expect, test } from '../fixtures';

/*
 * A whole Bar session with the production registry and the default layout,
 * from `index.html`: boot, every widget populated, Tiling and media activity
 * interleaved, a layout reload that reorders and removes widgets, a theme
 * switch, the menu bar appearing and disappearing. No console error and no
 * unanswered command.
 */

const PRODUCTION = '/index.html?surface=bar';
const ARTWORK = 'c'.repeat(64);

const item = (id: string, widget: BarLayoutItem['widget']): BarLayoutItem =>
  ({
    id,
    widget,
    options:
      widget === 'clock'
        ? { show_date: true, show_seconds: true, hour_cycle: 'h23' }
        : widget === 'cpu'
          ? { temperature_alert: 85 }
          : {},
  }) as BarLayoutItem;

/** The default `bar.toml` layout, as the host publishes it. */
const DEFAULT_LAYOUT: BarLayoutView = {
  start: [item('spaces', 'spaces')],
  center: [item('media', 'media')],
  end: [
    item('weather', 'weather'),
    item('cpu', 'cpu'),
    item('battery', 'battery'),
    item('keep_awake', 'keep_awake'),
    item('wifi', 'wifi'),
    item('clock', 'clock'),
  ],
};

/** The reloaded layout: Clock moves to the start, Weather and Battery are removed. */
const RELOADED_LAYOUT: BarLayoutView = {
  start: [item('clock', 'clock'), item('spaces', 'spaces')],
  center: [item('media', 'media')],
  end: [item('wifi', 'wifi'), item('keep_awake', 'keep_awake'), item('cpu', 'cpu')],
};

const app = (key: string, appName: string, title = ''): StripApp => ({ key, appName, title });
const GHOSTTY = app('11', 'Ghostty', 'zsh');
const CODE = app('12', 'Code', 'stache');
const SAFARI = app('21', 'Safari', 'Docs');

const strip = (
  sequence: string,
  focusedWorkspace: string,
  apps: StripApp[],
  focusedApp: StripApp | null,
): TilingStripView_Serialize => ({
  kind: 'available',
  generation: '5',
  sequence,
  workspaces: USER_WORKSPACES.map((name) => ({
    name,
    focused: name === focusedWorkspace,
    inactive: false,
  })),
  apps,
  focusedApp,
  focusedWorkspace,
  refusedFocus: null,
});

const track = (overrides: Partial<MediaReading_Serialize> = {}): MediaReading_Serialize => ({
  title: 'Song',
  artist: 'Band',
  album: null,
  bundleIdentifier: 'com.spotify.client',
  mediaType: null,
  playing: true,
  artworkId: null,
  ...overrides,
});

/** The Bar as Bar publishes it; each call is the next state. */
class Session {
  sequence = 1;
  view: BarView_Serialize = {
    generation: '1',
    sequence: '1',
    configRevision: '1',
    panel: 'loaded',
    menu: { state: 'hidden', reason: null },
    frame: null,
    height: 32,
    padding: 8,
    layout: DEFAULT_LAYOUT,
    detail: null,
    cpu: { usage: 23, temperature: 61 },
    battery: { percentage: 64, state: 'Discharging' },
    wifi: { status: 'Connected', networkName: 'Home', signalStrength: -58 },
    media: track(),
    weatherEpoch: '0',
    keepAwake: { active: true, paused: false, enabled: true },
    tiling: strip('10', 'terminal', [GHOSTTY, CODE], GHOSTTY),
  };

  private readonly host: FakeHost;

  constructor(host: FakeHost) {
    this.host = host;
  }

  snapshot(): BarSnapshotOutcome_Serialize {
    return { kind: 'available', view: this.view };
  }

  async update(changes: Partial<BarView_Serialize>) {
    this.sequence += 1;
    this.view = { ...this.view, ...changes, sequence: String(this.sequence) };
    await this.host.emitBarChanged({
      generation: '1',
      sequence: String(this.sequence),
      resnapshot: false,
      outcome: this.snapshot(),
    });
  }
}

const itemsOf = (page: Page, zone: string) =>
  page
    .locator(`[data-zone="${zone}"] [data-item]`)
    .evaluateAll((slots) => slots.map((slot) => (slot as HTMLElement).dataset.item));
const slot = (page: Page, id: string) => page.locator(`[data-item="${id}"]`);
const media = (page: Page) => slot(page, 'media').getByRole('button');
/* The focused app is the item without an `aria-label`; a leaving one may still be fading out. */
const focused = (page: Page) =>
  slot(page, 'spaces').locator('[data-app] > button:not([aria-label])').allTextContents();
const placement = (page: Page) =>
  page.locator('[data-surface="bar"]').evaluate((element) => {
    const style = getComputedStyle(element);

    return { y: new DOMMatrix(style.transform).m42, opacity: Number(style.opacity) };
  });
const background = (page: Page, id: string) =>
  slot(page, id)
    .getByRole('button')
    .first()
    .evaluate((element) => getComputedStyle(element).backgroundColor);

/* Catppuccin `crust`: an item at rest. */
const CRUST = { light: 'rgb(220, 224, 232)', dark: 'rgb(17, 17, 27)' };

test.describe('Bar session', () => {
  test.use({ viewport: { width: 2400, height: 200 } });

  test('boot, activity, layout reload, theme switch and menu bar', async ({
    page,
    host,
    consoleErrors,
  }) => {
    const session = new Session(host);
    const remote: string[] = [];

    await page.route(
      (url) => url.hostname !== 'localhost' && url.hostname !== '127.0.0.1',
      async (route) => {
        remote.push(route.request().url());
        await route.fulfill({
          contentType: 'application/json',
          body: JSON.stringify({ currentConditions: { feelslike: 17.2, icon: 'rain' } }),
        });
      },
    );
    host
      .answer('bar_snapshot', () => session.snapshot())
      .answer('bar_weather', {
        kind: 'ready',
        weather: {
          configRevision: '1',
          provider: 'auto',
          apiKey: 'SESSION-KEY',
          location: { source: 'defaultLocation', query: 'Porto' },
          refreshIntervalMinutes: 20,
        },
      })
      .answer('bar_media_artwork', { kind: 'ready', artwork: WARM_PNG })
      .answer('bar_action', { kind: 'completed' })
      .answer('bar_tiling_action', { kind: 'applied' });
    await page.emulateMedia({ colorScheme: 'dark' });
    await page.goto(PRODUCTION);

    // Boot: the default layout with every widget populated.
    await expect.poll(() => itemsOf(page, 'start')).toEqual(['spaces']);
    await expect.poll(() => itemsOf(page, 'center')).toEqual(['media']);
    await expect
      .poll(() => itemsOf(page, 'end'))
      .toEqual(['weather', 'cpu', 'battery', 'keep_awake', 'wifi', 'clock']);
    await expect.poll(() => focused(page)).toEqual(['Ghostty']);
    await expect(media(page)).toHaveText('Song - Band');
    await expect(slot(page, 'weather').getByRole('button')).toHaveText('18°C');
    await expect(slot(page, 'cpu').getByRole('button')).toHaveText('23%61°C');
    await expect(slot(page, 'battery').getByRole('button')).toHaveText('64% (Discharging)');
    await expect(slot(page, 'keep_awake').getByRole('button')).toHaveAccessibleName(
      'Keep Awake: on',
    );
    await expect(slot(page, 'wifi').getByRole('button')).toHaveText('Home');
    await expect(slot(page, 'clock').getByRole('button')).toHaveText(
      /^\w{3} \w{3} \d{2} \d{2}:\d{2}:\d{2}$/,
    );
    expect(await background(page, 'cpu')).toBe(CRUST.dark);

    // Tiling and media activity, interleaved.
    await session.update({ media: track({ artworkId: ARTWORK }) });
    await expect(slot(page, 'media').locator('img')).toHaveAttribute(
      'src',
      `data:image/png;base64,${WARM_PNG}`,
    );
    await session.update({ tiling: strip('11', 'terminal', [GHOSTTY, CODE], CODE) });
    await expect.poll(() => focused(page)).toEqual(['Code']);
    await session.update({ media: track({ artworkId: ARTWORK, playing: false }) });
    await expect(media(page)).toHaveText('Paused: Song - Band');
    await session.update({ tiling: strip('12', 'browser', [SAFARI], SAFARI) });
    await expect.poll(() => focused(page)).toEqual(['Safari']);
    await session.update({ media: track({ title: 'Next', artworkId: ARTWORK }) });
    await expect(media(page)).toHaveText('Next - Band');

    // Clicks reach the host.
    await media(page).click();
    await slot(page, 'spaces').getByRole('button', { name: 'terminal' }).click();
    await expect
      .poll(() => host.commands().filter((command) => command.startsWith('bar_')))
      .toEqual(expect.arrayContaining(['bar_action', 'bar_tiling_action']));

    // Layout reload: Clock moves to the start, Weather and Battery leave, the end reorders.
    await session.update({ layout: RELOADED_LAYOUT, configRevision: '2' });
    await expect.poll(() => itemsOf(page, 'start')).toEqual(['clock', 'spaces']);
    await expect.poll(() => itemsOf(page, 'end')).toEqual(['wifi', 'keep_awake', 'cpu']);
    await expect(slot(page, 'weather')).toHaveCount(0);
    await expect(slot(page, 'battery')).toHaveCount(0);
    await expect.poll(() => focused(page)).toEqual(['Safari']);
    await expect(media(page)).toHaveText('Next - Band');

    // Theme switch.
    await page.emulateMedia({ colorScheme: 'light' });
    await expect.poll(() => background(page, 'cpu')).toBe(CRUST.light);

    // The menu bar appears, then disappears, while Tiling keeps publishing.
    await session.update({ menu: { state: 'visible', reason: null } });
    await expect.poll(() => placement(page)).toEqual({ y: 32, opacity: 0 });
    await session.update({ tiling: strip('13', 'terminal', [GHOSTTY], GHOSTTY) });
    await session.update({ menu: { state: 'hidden', reason: null } });
    await expect.poll(() => placement(page)).toEqual({ y: 0, opacity: 1 });
    await expect.poll(() => focused(page)).toEqual(['Ghostty']);

    expect(remote.every((url) => url.startsWith('https://weather.visualcrossing.com/'))).toBe(true);
    expect(host.unanswered).toEqual([]);
    expect(consoleErrors).toEqual([]);
  });
});
