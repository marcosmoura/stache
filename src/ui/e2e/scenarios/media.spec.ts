import type { Page } from '@playwright/test';

import type {
  BarArtworkOutcome_Serialize,
  BarSnapshotOutcome_Serialize,
  MediaReading_Serialize,
} from '../../src/ipc/bindings.gen';
import { COOL_PNG, WARM_PNG } from '../../tests/png';
import { expect, test } from '../fixtures';

/*
 * The Media widget with the production registry, from `index.html`: a track
 * plays, its artwork arrives, it pauses, the next track's artwork arrives late,
 * playback stops and starts again, and the item is clicked.
 */

const PRODUCTION = '/index.html?surface=bar';
const A = 'a'.repeat(64);
const B = 'b'.repeat(64);

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

function available(
  sequence: string,
  media: MediaReading_Serialize | null,
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
      layout: { start: [], center: [{ id: 'media', widget: 'media', options: {} }], end: [] },
      detail: null,
      cpu: null,
      battery: null,
      wifi: null,
      media,
      weatherEpoch: '0',
      keepAwake: null,
      tiling: null,
    },
  };
}

const changed = (sequence: string, media: MediaReading_Serialize | null) => ({
  generation: '1',
  sequence,
  resnapshot: false,
  outcome: available(sequence, media),
});

const center = (page: Page) => page.locator('[data-zone="center"]');
const artwork = (page: Page) => center(page).locator('img');
const url = (base64: string) => `data:image/png;base64,${base64}`;

test.describe('Media', () => {
  test.use({ viewport: { width: 2400, height: 200 } });

  test('plays, pauses, changes track with late artwork, stops, starts again and clicks', async ({
    page,
    host,
    consoleErrors,
  }) => {
    let releaseB: (outcome: BarArtworkOutcome_Serialize) => void = () => undefined;

    host.answer('bar_media_artwork', (args: unknown) => {
      const { id } = args as { id: string };

      if (id === A) {
        return { kind: 'ready', artwork: WARM_PNG };
      }

      return new Promise<BarArtworkOutcome_Serialize>((resolve) => {
        releaseB = resolve;
      });
    });
    host.answer('bar_action', { kind: 'completed' });
    host.answerBarSnapshot(available('1', track()));
    await page.goto(PRODUCTION);

    const button = center(page).getByRole('button');

    // Play: label and player icon, no image until the artwork arrives.
    await expect(button).toHaveText('Song - Band');
    await expect(artwork(page)).toHaveCount(0);
    expect((await button.boundingBox())!.height).toBe(32);

    // Artwork delivered.
    await host.emitBarChanged(changed('2', track({ artworkId: A })));
    await expect(artwork(page)).toHaveAttribute('src', url(WARM_PNG));

    // Pause.
    await host.emitBarChanged(changed('3', track({ artworkId: A, playing: false })));
    await expect(button).toHaveText('Paused: Song - Band');
    await expect(artwork(page)).toHaveAttribute('src', url(WARM_PNG));

    // Next track: first without an id, then an id whose artwork arrives later.
    await host.emitBarChanged(changed('4', track({ title: 'Next', artist: '' })));
    await expect(button).toHaveText('Next');
    await expect(artwork(page)).toHaveAttribute('src', url(WARM_PNG));
    await host.emitBarChanged(changed('5', track({ title: 'Next', artist: '', artworkId: B })));
    await expect
      .poll(() => host.calls.filter((call) => call.command === 'bar_media_artwork').length)
      .toBe(2);
    await expect(artwork(page)).toHaveAttribute('src', url(WARM_PNG));
    releaseB({ kind: 'ready', artwork: COOL_PNG });
    await expect(artwork(page)).toHaveAttribute('src', url(COOL_PNG));

    // Stop: hidden.
    await host.emitBarChanged(changed('6', null));
    await expect(center(page).getByRole('button')).toHaveCount(0);

    // Start again: the last image is still there.
    await host.emitBarChanged(changed('7', track({ title: 'Again' })));
    await expect(button).toHaveText('Again - Band');
    await expect(artwork(page)).toHaveAttribute('src', url(COOL_PNG));

    // Click.
    await button.click();
    await expect
      .poll(() => host.calls.filter((call) => call.command === 'bar_action'))
      .toEqual([{ command: 'bar_action', args: { action: 'media' } }]);

    expect(
      host.calls.filter((call) => call.command === 'bar_media_artwork').map((call) => call.args),
    ).toEqual([{ id: A }, { id: B }]);
    expect(consoleErrors).toEqual([]);
  });

  test('a long title on a narrow Bar never squeezes or overlaps the end zone', async ({
    page,
    host,
  }) => {
    await page.setViewportSize({ width: 1400, height: 200 });
    host.answer('bar_media_artwork', { kind: 'ready', artwork: WARM_PNG });

    const base = available(
      '1',
      track({
        title: 'How I Met Your Mother - S07E24 - The Magician\u2019s Code: Part Two',
        artist: 'A Very Long Artist Name That Keeps Going',
        playing: false,
        artworkId: A,
      }),
    );

    if (base.kind !== 'available') {
      throw new Error('fixture');
    }

    host.answerBarSnapshot({
      ...base,
      view: {
        ...base.view,
        layout: {
          start: [],
          center: [{ id: 'media', widget: 'media', options: {} }],
          end: [
            { id: 'cpu', widget: 'cpu', options: { temperature_alert: 85 } },
            { id: 'battery', widget: 'battery', options: {} },
            { id: 'wifi', widget: 'wifi', options: {} },
            {
              id: 'clock',
              widget: 'clock',
              options: { show_date: true, show_seconds: true, hour_cycle: 'h23' },
            },
          ],
        },
        cpu: { usage: 26, temperature: 66 },
        battery: { percentage: 28, state: 'Discharging' },
        wifi: { status: 'Connected', networkName: 'Silence of the LANs', signalStrength: -50 },
      },
    });
    await page.goto(PRODUCTION);

    const media = center(page).getByRole('button');
    const endItems = page.locator('[data-zone="end"] button');

    await expect(endItems).toHaveCount(4);
    await expect(artwork(page)).toHaveCount(1);
    await expect(center(page).locator('[data-state="scrolling"]')).toHaveCount(1);

    const boxes = await endItems.evaluateAll((items) =>
      items.map((item) => {
        const rect = item.getBoundingClientRect();

        return { left: rect.left, right: rect.right, height: rect.height };
      }),
    );
    const mediaBox = (await media.boundingBox())!;
    const imageBox = (await artwork(page).boundingBox())!;

    // Every item keeps the Bar height: nothing wraps onto a second line.
    expect(mediaBox.height).toBe(32);
    expect(boxes.map((box) => box.height)).toEqual([32, 32, 32, 32]);
    // The end zone keeps its content inside the Bar and Media stops before it.
    expect(boxes.at(-1)!.right).toBeLessThanOrEqual(1400);
    expect(mediaBox.x).toBeGreaterThanOrEqual(0);
    expect(mediaBox.x + mediaBox.width).toBeLessThanOrEqual(boxes[0]!.left);
    // Wi-Fi shows its whole name and the Clock stays on one line.
    await expect(endItems.nth(2)).toHaveText('Silence of the LANs');
    await expect(endItems.nth(3)).toHaveText(/^\w{3} \w{3} \d{2} \d{2}:\d{2}:\d{2}$/);
    // The artwork sits 2px from the item's leading edge and is centered in it.
    expect(imageBox.x - mediaBox.x).toBe(2);
    expect(imageBox.y - mediaBox.y).toBe(4);
  });
});
