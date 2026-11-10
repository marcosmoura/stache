import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { buildScenarios } from '../../../../tests/bar-parity/scenarios';
import {
  PNG_PIXEL,
  availableOutcome,
  barData,
  unavailableOutcome,
} from '../../../../tests/barFixtures';
import { jsonResponse, OM_HOST, omBody, onHost, stubFetch } from '../../../../tests/weatherFetch';
import type {
  BarChanged,
  BarSnapshotOutcome,
  MediaReading_Serialize,
} from '../../../sdk/bindings.gen';

import { Bar } from './Bar';
import { ABSENT_BAR_DATA, barDataFromOutcome, type BarData } from './Bar.data';
import { BarDataProvider } from './BarDataProvider';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(), once: vi.fn(), emit: vi.fn() }));

const ID_A = 'a'.repeat(64);
const ID_B = 'b'.repeat(64);
const ID_C = 'c'.repeat(64);

function reading(overrides: Partial<MediaReading_Serialize> = {}): MediaReading_Serialize {
  return {
    title: 'Song',
    artist: 'Band',
    album: null,
    bundleIdentifier: 'com.spotify.client',
    mediaType: null,
    playing: true,
    artworkId: null,
    ...overrides,
  };
}

/** A second valid PNG that differs from `PNG_PIXEL`. */
function otherPng() {
  const canvas = document.createElement('canvas');

  canvas.width = 2;
  canvas.height = 2;
  canvas.getContext('2d')?.fillRect(0, 0, 2, 2);

  return canvas.toDataURL('image/png').split(',')[1];
}

type Handler = (event: { payload: BarChanged }) => void;

function snapshotIs(outcome: BarSnapshotOutcome) {
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'bar_snapshot') {
      return outcome;
    }

    throw new Error(`Unexpected command: ${command}`);
  }) as never);
}

async function renderBridgeBar() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  });
  const screen = await render(
    <QueryClientProvider client={client}>
      <Bar />
    </QueryClientProvider>,
  );

  screen.container.style.height = '28px';

  return screen;
}

async function renderFixtureBar(data: BarData) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  });
  const screen = await render(
    <QueryClientProvider client={client}>
      <BarDataProvider data={data}>
        <Bar />
      </BarDataProvider>
    </QueryClientProvider>,
  );

  screen.container.style.height = '28px';

  return screen;
}

function statusHtml(container: HTMLElement) {
  return container.querySelector('[data-testid="status-container"]')?.innerHTML ?? '';
}

// The status row fades in and the scrolling label measures after layout, so a fixture read taken
// right after mount can still be mid-transition. Wait until two reads 50 ms apart agree.
async function settledStatusHtml(container: HTMLElement) {
  let previous = statusHtml(container);

  for (let attempt = 0; attempt < 40; attempt += 1) {
    await new Promise((resolve) => {
      setTimeout(resolve, 50);
    });

    const current = statusHtml(container);

    if (current === previous && !current.includes('opacity: 0;')) {
      return current;
    }

    previous = current;
  }

  return previous;
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
  vi.mocked(listen).mockResolvedValue(vi.fn());
  vi.useFakeTimers({ toFake: ['Date', 'setInterval', 'clearInterval'] });
  vi.setSystemTime(new Date(2026, 0, 5, 9, 4, 3));
});

afterEach(() => {
  vi.useRealTimers();
});

describe('barDataFromOutcome', () => {
  test('is absent for every answer that is not an available view', () => {
    expect(barDataFromOutcome(undefined)).toBe(ABSENT_BAR_DATA);
    expect(barDataFromOutcome({ kind: 'unauthorized' })).toBe(ABSENT_BAR_DATA);
    expect(barDataFromOutcome(unavailableOutcome('2'))).toBe(ABSENT_BAR_DATA);
  });

  test('maps BarView.media onto the media payload without artwork', () => {
    const reading = {
      title: 'Song',
      artist: 'Band',
      album: 'Record',
      bundleIdentifier: 'com.spotify.client',
      mediaType: 'music',
      playing: false,
      artworkId: ID_A,
    };

    expect(barDataFromOutcome(availableOutcome({ media: reading })).media).toEqual({
      title: 'Song',
      artist: 'Band',
      album: 'Record',
      bundleIdentifier: 'com.spotify.client',
      mediaType: 'music',
      playing: false,
    });
    expect(
      barDataFromOutcome(
        availableOutcome({ media: { ...reading, album: null, mediaType: null, artist: '' } }),
      ).media,
    ).toEqual({
      title: 'Song',
      artist: '',
      album: null,
      bundleIdentifier: 'com.spotify.client',
      mediaType: null,
      playing: false,
    });
  });

  test('keeps each unresolved provider absent and maps present readings value for value', () => {
    expect(barDataFromOutcome(availableOutcome())).toEqual(ABSENT_BAR_DATA);

    const data = barDataFromOutcome(
      availableOutcome({
        cpu: { usage: 5, temperature: null },
        battery: { percentage: 40, state: 'Full' },
        wifi: { status: 'Connected', networkName: null, signalStrength: null },
      }),
    );

    expect(data.cpu).toEqual({ usage: 5, temperature: null });
    expect(data.battery).toEqual({ percentage: 40, state: 'Full' });
    expect(data.wifi).toEqual({ status: 'Connected', networkName: null, signalStrength: null });
    expect(data.media).toBeNull();
    expect(data.keepAwake).toBeUndefined();
  });
});

describe('Keep Awake mapping', () => {
  test('maps BarView.keepAwake: null or absent is unresolved, otherwise the active flag', () => {
    expect(barDataFromOutcome(availableOutcome({ keepAwake: null })).keepAwake).toBeUndefined();
    expect(barDataFromOutcome(availableOutcome()).keepAwake).toBeUndefined();
    expect(
      barDataFromOutcome(
        availableOutcome({ keepAwake: { active: true, paused: false, enabled: true } }),
      ).keepAwake,
    ).toBe(true);
    expect(
      barDataFromOutcome(
        availableOutcome({ keepAwake: { active: false, paused: true, enabled: true } }),
      ).keepAwake,
    ).toBe(false);
    expect(
      barDataFromOutcome(
        availableOutcome({ keepAwake: { active: false, paused: false, enabled: false } }),
      ).keepAwake,
    ).toBe(false);
  });
});

describe('Keep Awake session mapping and click', () => {
  test('carries only the paused/enabled facts the click is based on', () => {
    expect(barDataFromOutcome(availableOutcome()).keepAwakeSession).toBeUndefined();
    expect(
      barDataFromOutcome(
        availableOutcome({ keepAwake: { active: false, paused: true, enabled: true } }),
      ).keepAwakeSession,
    ).toEqual({ paused: true, enabled: true });
  });

  test('a click from the bridge view sends the observed action and the DOM follows only the view', async () => {
    vi.mocked(invoke).mockImplementation((async (command: string) => {
      if (command === 'bar_snapshot') {
        return availableOutcome({ keepAwake: { active: true, paused: false, enabled: true } });
      }

      if (command === 'bar_session_action') {
        return { kind: 'applied' };
      }

      throw new Error(`Unexpected command: ${command}`);
    }) as never);

    const screen = await renderBridgeBar();

    await expect
      .poll(() =>
        screen.container.querySelector('[data-test-state]')?.getAttribute('data-test-state'),
      )
      .toBe('awake');
    await screen.container.querySelector<HTMLButtonElement>('[data-test-state]')?.click();

    expect(vi.mocked(invoke)).toHaveBeenCalledWith('bar_session_action', {
      module: 'keepAwake',
      action: 'pause',
    });
    // An applied answer alone never repaints: still awake until a view says otherwise.
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(
      screen.container.querySelector('[data-test-state]')?.getAttribute('data-test-state'),
    ).toBe('awake');
  });
});

describe('Bar provider data from the bridge', () => {
  test('renders CPU, battery and Wi-Fi from the available view and follows events', async () => {
    snapshotIs(
      availableOutcome({
        sequence: '1',
        cpu: { usage: 23, temperature: 48 },
        battery: { percentage: 80, state: 'Discharging' },
        wifi: { status: 'Connected', networkName: null, signalStrength: -55 },
      }),
    );

    const screen = await renderBridgeBar();

    await expect.poll(() => screen.container.textContent).toContain('23%');
    expect(screen.container.textContent).toContain('48°C');
    expect(screen.container.textContent).toContain('80%');

    const handler = vi.mocked(listen).mock.calls.at(-1)?.[1] as Handler;

    handler({
      payload: {
        generation: '1',
        sequence: '2',
        resnapshot: false,
        outcome: availableOutcome({
          sequence: '2',
          cpu: { usage: 61, temperature: null },
          battery: null,
          wifi: { status: 'Off', networkName: null, signalStrength: null },
        }),
      },
    });

    await expect.poll(() => screen.container.textContent).toContain('61%');
    expect(screen.container.textContent).not.toContain('°C');
    expect(screen.container.textContent).not.toContain('80%');
  });

  test('renders no provider component before the first reading', async () => {
    snapshotIs(availableOutcome({ sequence: '1' }));

    const screen = await renderBridgeBar();

    await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
    expect(
      screen.container.querySelectorAll('[data-testid="status-container"] button'),
    ).toHaveLength(1);
  });

  test('renders the weather label from bar_weather and the provider', async () => {
    const harness = stubFetch([
      { match: onHost(OM_HOST), respond: () => jsonResponse(omBody(61, 1, 20.2)) },
    ]);
    const answer = {
      kind: 'ready',
      weather: {
        configRevision: '1',
        provider: 'openMeteo',
        apiKey: null,
        refreshIntervalMinutes: 20,
        location: { source: 'coreLocation', latitude: 1, longitude: 2, reducedAccuracy: false },
      },
    };

    try {
      vi.mocked(invoke).mockImplementation((async (command: string) => {
        if (command === 'bar_snapshot') {
          return availableOutcome({ sequence: '1' });
        }

        if (command === 'bar_weather') {
          return answer;
        }

        throw new Error(`Unexpected command: ${command}`);
      }) as never);

      const screen = await renderBridgeBar();

      await expect.element(screen.getByText('21°C')).toBeVisible();
      expect(harness.unexpected()).toEqual([]);
    } finally {
      harness.restore();
    }
  });

  // The seam shape is value-identical, so every part 04 branch renders the same DOM through the
  // bridge as through the fixture provider.
  const scenarios = buildScenarios('').filter(
    (scenario) =>
      scenario.mode === 'bar' &&
      scenario.data.cpu !== null &&
      (scenario.data.battery == null || typeof scenario.data.battery.state === 'string'),
  );

  test.each(scenarios.map((scenario) => [scenario.name, scenario.data] as const))(
    'matches the fixture rendering for %s',
    async (_name, data) => {
      const providers = barData({
        cpu: data.cpu,
        battery: data.battery,
        wifi: data.wifi,
      });

      snapshotIs(availableOutcome({ sequence: '1' }));

      const reference = await renderFixtureBar(providers);
      const expected = await settledStatusHtml(reference.container);

      await reference.unmount();
      vi.mocked(invoke).mockClear();
      snapshotIs(
        availableOutcome({
          sequence: '1',
          cpu: providers.cpu ?? undefined,
          battery: providers.battery ?? undefined,
          wifi: providers.wifi ?? undefined,
        }),
      );

      const bridged = await renderBridgeBar();

      await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
      await expect.poll(() => statusHtml(bridged.container)).toBe(expected);
    },
  );
});

describe('Media from the bridge', () => {
  const mediaImage = (container: HTMLElement) =>
    container.querySelector<HTMLImageElement>('[data-testid="media-container"] img');
  const mediaText = (container: HTMLElement) =>
    container.querySelector('[data-testid="media-container"]')?.textContent ?? null;

  function emit(media: MediaReading_Serialize | null, sequence: string) {
    const handler = vi.mocked(listen).mock.calls.at(-1)?.[1] as Handler;

    handler({
      payload: {
        generation: '1',
        sequence,
        resnapshot: false,
        outcome: availableOutcome({ sequence, media }),
      },
    });
  }

  function mockMedia(
    media: MediaReading_Serialize | null,
    artwork: (id: string) => Promise<unknown>,
  ) {
    vi.mocked(invoke).mockImplementation((async (command: string, args?: { id: string }) => {
      if (command === 'bar_snapshot') {
        return availableOutcome({ sequence: '1', media });
      }

      if (command === 'bar_media_artwork') {
        return artwork((args as { id: string }).id);
      }

      throw new Error(`Unexpected command: ${command}`);
    }) as never);
  }

  const artworkCalls = () =>
    vi.mocked(invoke).mock.calls.filter(([command]) => command === 'bar_media_artwork');

  test('renders the label, fetches the artwork by id and shows it', async () => {
    mockMedia(reading({ artworkId: ID_A }), async () => ({ kind: 'ready', artwork: PNG_PIXEL }));

    const screen = await renderBridgeBar();

    await expect.poll(() => mediaText(screen.container)).toContain('Song - Band');
    await expect
      .poll(() => mediaImage(screen.container)?.src)
      .toBe(`data:image/png;base64,${PNG_PIXEL}`);
    expect(artworkCalls()).toEqual([['bar_media_artwork', { id: ID_A }]]);
  });

  test('shows the Paused prefix and hides Media for null and for an unavailable snapshot', async () => {
    mockMedia(reading({ playing: false }), async () => ({ kind: 'missing' }));

    const screen = await renderBridgeBar();

    await expect.poll(() => mediaText(screen.container)).toContain('Paused:');
    emit(null, '2');
    await expect.poll(() => mediaText(screen.container)).toBeNull();
    emit(reading(), '3');
    await expect.poll(() => mediaText(screen.container)).toContain('Song - Band');

    const handler = vi.mocked(listen).mock.calls.at(-1)?.[1] as Handler;

    handler({
      payload: {
        generation: '1',
        sequence: '4',
        resnapshot: false,
        outcome: unavailableOutcome('4'),
      },
    });
    await expect.poll(() => mediaText(screen.container)).toBeNull();
  });

  test('a track change without artwork keeps the previous image; a new id replaces it', async () => {
    const second = otherPng();

    mockMedia(reading({ artworkId: ID_A }), async (id) => ({
      kind: 'ready',
      artwork: id === ID_A ? PNG_PIXEL : second,
    }));

    const screen = await renderBridgeBar();

    await expect.poll(() => mediaImage(screen.container)?.src).toContain(PNG_PIXEL);

    emit(reading({ title: 'Next', artworkId: null }), '2');
    await expect.poll(() => mediaText(screen.container)).toContain('Next - Band');
    expect(mediaImage(screen.container)?.src).toContain(PNG_PIXEL);

    emit(reading({ title: 'Next', artworkId: ID_B }), '3');
    await expect.poll(() => mediaImage(screen.container)?.src).toContain(second);
    expect(artworkCalls()).toHaveLength(2);
  });

  test('a pending fetch keeps the image and a stale answer for an older id never replaces a newer one', async () => {
    const newer = otherPng();
    let resolveB: (value: unknown) => void = () => {};

    mockMedia(reading({ artworkId: ID_A }), (id) => {
      if (id === ID_A) {
        return Promise.resolve({ kind: 'ready', artwork: PNG_PIXEL });
      }

      if (id === ID_B) {
        return new Promise((resolve) => {
          resolveB = resolve;
        });
      }

      return Promise.resolve({ kind: 'ready', artwork: newer });
    });

    const screen = await renderBridgeBar();

    await expect.poll(() => mediaImage(screen.container)?.src).toContain(PNG_PIXEL);

    emit(reading({ artworkId: ID_B }), '2');
    await expect.poll(() => artworkCalls()).toHaveLength(2);
    expect(mediaImage(screen.container)?.src).toContain(PNG_PIXEL);

    emit(reading({ artworkId: ID_C }), '3');
    await expect.poll(() => mediaImage(screen.container)?.src).toContain(newer);

    resolveB({ kind: 'ready', artwork: PNG_PIXEL });
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(mediaImage(screen.container)?.src).toContain(newer);
  });

  test.each([
    ['missing', async () => ({ kind: 'missing' })],
    ['unavailable', async () => ({ kind: 'unavailable', reason: 'notRunning', detail: null })],
    ['unauthorized', async () => ({ kind: 'unauthorized' })],
    [
      'a rejection',
      async () => {
        throw new Error('ipc down');
      },
    ],
  ])('%s shows no artwork and adds nothing', async (_name, answer) => {
    mockMedia(reading({ artworkId: ID_A }), answer);

    const screen = await renderBridgeBar();

    await expect.poll(() => mediaText(screen.container)).toContain('Song - Band');
    await expect.poll(() => artworkCalls()).toHaveLength(1);
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(mediaImage(screen.container)).toBeNull();
    expect(
      screen.container.querySelector('[data-testid="media-container"]')?.children,
    ).toHaveLength(2);
  });

  test('clicking Media sends only the media action', async () => {
    mockMedia(reading(), async () => ({ kind: 'missing' }));

    const screen = await renderBridgeBar();

    await expect.poll(() => mediaText(screen.container)).toContain('Song - Band');
    await screen.getByTestId('media-container').click();
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('bar_action', { action: 'media' });
  });
});
