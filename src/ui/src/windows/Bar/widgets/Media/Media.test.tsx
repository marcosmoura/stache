import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import type { BarArtworkOutcome_Serialize, MediaReading_Serialize } from '@/ipc';

import { renderWithSnapshot } from '../../../../../tests/barSnapshot';
import { barAvailable, barUnavailable } from '../../../../../tests/fixtures';
import { mockHost } from '../../../../../tests/ipc';
import { COOL_PNG, INVALID_PNG, WARM_PNG } from '../../../../../tests/png';

import { Media } from './Media';
import { artworkUrl } from './Media.query';

afterEach(async () => {
  vi.restoreAllMocks();
  await page.viewport(414, 896);
});

const id = (char: string) => char.repeat(64);

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

const label = () => document.querySelector<HTMLElement>('[data-state]');
const image = () => document.querySelector<HTMLImageElement>('img');
const shown = () => image()?.getAttribute('src') ?? null;
const settle = (ms = 30) => new Promise((done) => setTimeout(done, ms));

type Deferred = { resolve: (outcome: BarArtworkOutcome_Serialize) => void };

/** A host whose artwork answers come from `table` or, for held ids, when released. */
function artworkHost(table: Record<string, BarArtworkOutcome_Serialize | 'hold'>) {
  const held = new Map<string, Deferred>();
  const host = mockHost()
    .answer('bar_action', { kind: 'completed' })
    .answer('bar_media_artwork', (args: unknown) => {
      const key = (args as { id: string }).id;
      const answer = table[key] ?? { kind: 'missing' };

      if (answer === 'hold') {
        return new Promise((resolve) => held.set(key, { resolve }));
      }

      return answer;
    });

  const release = (key: string, outcome: BarArtworkOutcome_Serialize) => {
    held.get(key)!.resolve(outcome);
  };

  return { host, release };
}

const ready = (artwork: string): BarArtworkOutcome_Serialize => ({ kind: 'ready', artwork });

describe('Media', () => {
  it('is hidden without media or while the Bar is not available', async () => {
    mockHost();
    const { screen, setSnapshot, setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: null }),
    );

    await settle();
    expect(screen.container.childElementCount).toBe(0);
    setView({ media: track() });
    await expect.element(screen.getByRole('button')).toBeVisible();
    setSnapshot(barUnavailable('200'));
    await vi.waitFor(() => expect(screen.container.childElementCount).toBe(0));
  });

  it('shows the label, the paused prefix and the title alone', async () => {
    mockHost();
    const { screen, setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track() }),
    );
    const button = screen.getByRole('button');

    await expect.element(button).toHaveTextContent('Song - Band');
    expect(button.element().textContent).not.toContain('Paused');
    setView({ media: track({ playing: false }) });
    await expect.element(button).toHaveTextContent('Paused: Song - Band');
    // The prefix is its own element, before the scrolling label.
    const prefix = button.element().querySelector('span:not([data-state] span)');

    expect(prefix?.textContent).toBe('Paused: ');
    expect(label()!.textContent).toBe('Song - Band');
    setView({ media: track({ artist: '', playing: true }) });
    await vi.waitFor(() => expect(button.element().textContent).toBe('Song'));
  });

  it('draws the player icon at 22px and no image without artwork', async () => {
    mockHost();
    const { screen } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ bundleIdentifier: 'com.example.player' }) }),
    );
    const svg = screen.getByRole('button').element().querySelector('svg')!;

    await vi.waitFor(() => expect(svg.getBoundingClientRect().width).toBe(22));
    expect(image()).toBeNull();
  });

  it('scrolls the label only on overflow, at most 560px wide', async () => {
    await page.viewport(2400, 200);
    mockHost();
    const { setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track() }),
    );

    await vi.waitFor(() => expect(label()?.dataset.state).toBe('static'));
    setView({ media: track({ title: 'A very long title '.repeat(10) }) });
    await vi.waitFor(() => expect(label()?.dataset.state).toBe('scrolling'));
    expect(label()!.offsetWidth).toBeLessThanOrEqual(560 + 8);
    expect(label()!.offsetWidth).toBeGreaterThan(400 + 8);
  });

  it('narrows the label to 400px in compact mode', async () => {
    await page.viewport(2036, 200);
    mockHost();
    await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ title: 'A very long title '.repeat(10) }) }),
    );

    await vi.waitFor(() => expect(label()?.dataset.state).toBe('scrolling'));
    expect(label()!.offsetWidth).toBeLessThanOrEqual(400 + 8);
  });

  it('sends bar_action("media") on click', async () => {
    const { host } = artworkHost({});
    const { screen } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track() }),
    );

    await screen.getByRole('button').click();
    await vi.waitFor(() => expect(host.callsOf('bar_action')).toHaveLength(1));
    expect(host.callsOf('bar_action')[0]!.args).toEqual({ action: 'media' });
  });
});

describe('Media artwork', () => {
  it('shows a decoded 24px rounded image before the icon', async () => {
    artworkHost({ [id('a')]: ready(WARM_PNG) });
    const { screen } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ artworkId: id('a') }) }),
    );

    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(WARM_PNG)));
    const img = image()!;
    const style = getComputedStyle(img);

    expect(img.getBoundingClientRect().width).toBe(24);
    expect(img.getBoundingClientRect().height).toBe(24);
    expect(style.borderTopLeftRadius).toBe('10px');
    expect(img.complete && img.naturalWidth).toBe(24);
    expect(img.nextElementSibling?.tagName.toLowerCase()).toBe('svg');
    expect(img.alt).toBe('Song - Band');
    await expect.element(screen.getByRole('button')).toBeVisible();
  });

  it('draws nothing for missing, unavailable, unauthorized or a rejected call', async () => {
    const { host } = artworkHost({
      [id('m')]: { kind: 'missing' },
      [id('u')]: { kind: 'unavailable', reason: 'stopped', detail: null },
      [id('z')]: { kind: 'unauthorized' },
    });
    const { screen, setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ artworkId: id('m') }) }),
    );

    for (const key of ['u', 'z']) {
      await settle();
      setView({ media: track({ artworkId: id(key) }) });
    }

    await vi.waitFor(() => expect(host.callsOf('bar_media_artwork')).toHaveLength(3));
    host.answer('bar_media_artwork', () => Promise.reject(new Error('ipc down')));
    setView({ media: track({ artworkId: id('r') }) });
    await vi.waitFor(() => expect(host.callsOf('bar_media_artwork')).toHaveLength(4));
    await settle();
    expect(image()).toBeNull();
    await expect.element(screen.getByRole('button')).toHaveTextContent('Song - Band');
  });

  it('fetches an id once, even when it returns', async () => {
    const { host } = artworkHost({ [id('a')]: ready(WARM_PNG), [id('b')]: ready(COOL_PNG) });
    const { setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ artworkId: id('a') }) }),
    );

    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(WARM_PNG)));
    setView({ media: track({ artworkId: id('b') }) });
    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(COOL_PNG)));
    setView({ media: track({ artworkId: id('a') }) });
    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(WARM_PNG)));
    expect(host.callsOf('bar_media_artwork').map((call) => call.args)).toEqual([
      { id: id('a') },
      { id: id('b') },
    ]);
  });

  it('keeps the image across a track change until the late artwork decodes', async () => {
    const { host, release } = artworkHost({ [id('a')]: ready(WARM_PNG), [id('b')]: 'hold' });
    const { setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ artworkId: id('a') }) }),
    );

    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(WARM_PNG)));
    setView({ media: track({ title: 'Next', artworkId: null }) });
    await settle();
    expect(shown()).toBe(artworkUrl(WARM_PNG));
    setView({ media: track({ title: 'Next', artworkId: id('b') }) });
    await vi.waitFor(() => expect(host.callsOf('bar_media_artwork')).toHaveLength(2));
    await settle();
    expect(shown()).toBe(artworkUrl(WARM_PNG));
    release(id('b'), ready(COOL_PNG));
    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(COOL_PNG)));
  });

  it('keeps the image after a failed fetch', async () => {
    artworkHost({
      [id('a')]: ready(WARM_PNG),
      [id('f')]: { kind: 'unavailable', reason: 'stopped', detail: null },
    });
    const { setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ artworkId: id('a') }) }),
    );

    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(WARM_PNG)));
    setView({ media: track({ artworkId: id('f') }) });
    await settle(60);
    expect(shown()).toBe(artworkUrl(WARM_PNG));
  });

  it('clears the image when a new one fails to decode', async () => {
    artworkHost({ [id('a')]: ready(WARM_PNG), [id('x')]: ready(INVALID_PNG) });
    const { setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ artworkId: id('a') }) }),
    );

    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(WARM_PNG)));
    setView({ media: track({ artworkId: id('x') }) });
    await vi.waitFor(() => expect(image()).toBeNull());
  });

  it('never shows an older response for a newer id', async () => {
    const { host, release } = artworkHost({ [id('a')]: 'hold', [id('b')]: 'hold' });
    const { setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ artworkId: id('a') }) }),
    );

    await vi.waitFor(() => expect(host.callsOf('bar_media_artwork')).toHaveLength(1));
    setView({ media: track({ artworkId: id('b') }) });
    await vi.waitFor(() => expect(host.callsOf('bar_media_artwork')).toHaveLength(2));
    release(id('a'), ready(WARM_PNG));
    await settle(60);
    expect(image()).toBeNull();
    release(id('b'), ready(COOL_PNG));
    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(COOL_PNG)));
  });

  it('keeps the newer image when an older one arrives late', async () => {
    const { host, release } = artworkHost({ [id('a')]: 'hold', [id('b')]: 'hold' });
    const { setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ artworkId: id('a') }) }),
    );

    await vi.waitFor(() => expect(host.callsOf('bar_media_artwork')).toHaveLength(1));
    setView({ media: track({ artworkId: id('b') }) });
    await vi.waitFor(() => expect(host.callsOf('bar_media_artwork')).toHaveLength(2));
    release(id('b'), ready(COOL_PNG));
    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(COOL_PNG)));
    release(id('a'), ready(WARM_PNG));
    await settle(60);
    expect(shown()).toBe(artworkUrl(COOL_PNG));
  });

  it('keeps the last image while hidden and shows it for the next track', async () => {
    artworkHost({ [id('a')]: ready(WARM_PNG) });
    const { screen, setView } = await renderWithSnapshot(
      <Media id="media" zone="center" options={{}} />,
      barAvailable('1', { media: track({ artworkId: id('a') }) }),
    );

    await vi.waitFor(() => expect(shown()).toBe(artworkUrl(WARM_PNG)));
    setView({ media: null });
    await vi.waitFor(() => expect(screen.container.childElementCount).toBe(0));
    setView({ media: track({ title: 'Again', artworkId: null }) });
    await expect.element(screen.getByRole('button')).toHaveTextContent('Again - Band');
    expect(shown()).toBe(artworkUrl(WARM_PNG));
  });
});
