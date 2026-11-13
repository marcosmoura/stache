import type {
  BarSnapshotOutcome_Serialize,
  MediaReading_Serialize,
} from '../../src/ipc/bindings.gen';
import { WARM_PNG } from '../../tests/png';
import { expect, test } from '../fixtures';

import { skipWithoutMapleMono } from './font';

/*
 * Baselines of the Media item (playing with artwork, paused, no artwork) from
 * production `index.html` with the fake host. Compared only with 'Maple Mono
 * NF' installed. Update with
 * `bunx playwright test src/ui/e2e/screenshots/media.spec.ts --update-snapshots`.
 */

const ARTWORK_ID = 'a'.repeat(64);

const STATES: Record<string, MediaReading_Serialize> = {
  playing: {
    title: 'Song',
    artist: 'Band',
    album: null,
    bundleIdentifier: 'com.spotify.client',
    mediaType: null,
    playing: true,
    artworkId: ARTWORK_ID,
  },
  paused: {
    title: 'Song',
    artist: 'Band',
    album: null,
    bundleIdentifier: 'com.spotify.client',
    mediaType: null,
    playing: false,
    artworkId: ARTWORK_ID,
  },
  'no-artwork': {
    title: 'Song',
    artist: '',
    album: null,
    bundleIdentifier: 'com.example.player',
    mediaType: null,
    playing: true,
    artworkId: null,
  },
};

const snapshot = (media: MediaReading_Serialize): BarSnapshotOutcome_Serialize => ({
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
});

test.use({ viewport: { width: 2400, height: 120 }, deviceScaleFactor: 2 });

for (const [state, media] of Object.entries(STATES)) {
  for (const scheme of ['dark', 'light'] as const) {
    test(`media ${state} (${scheme})`, async ({ page, host }) => {
      host.answerBarSnapshot(snapshot(media));
      host.answer('bar_media_artwork', { kind: 'ready', artwork: WARM_PNG });
      await page.emulateMedia({ colorScheme: scheme });
      await page.goto('/index.html?surface=bar');
      await skipWithoutMapleMono(page);

      const center = page.locator('[data-zone="center"]');
      const button = center.getByRole('button');

      await expect(button).toHaveText(state === 'paused' ? 'Paused: Song - Band' : /^Song/);
      await expect(button).toHaveCSS('opacity', '1');
      await expect(center.locator('img')).toHaveCount(media.artworkId ? 1 : 0);
      await page.evaluate(() => document.fonts.ready.then(() => undefined));
      await expect(center).toHaveScreenshot(`media-${state}-${scheme}.png`);
    });
  }
}
