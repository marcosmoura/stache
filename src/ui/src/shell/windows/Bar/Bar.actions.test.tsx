import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';

import { barData } from '../../../../tests/barFixtures';
import { renderWithBarData } from '../../../../tests/renderBar';

import { Battery } from './Battery';
import { Clock } from './Clock';
import { Cpu } from './Cpu';
import { KeepAwake } from './KeepAwake';
import { Media } from './Media';
import { Weather } from './Weather';
import { Wifi } from './Wifi';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const data = barData({
  cpu: { usage: 30, temperature: 50 },
  battery: { percentage: 80, state: 'Discharging' },
  wifi: { status: 'Connected', networkName: 'Home', signalStrength: -50 },
  weather: {
    isConfigured: true,
    isLoading: false,
    currentConditions: { feelslike: 20, icon: 'clearDay' },
  },
});

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockResolvedValue({ kind: 'completed' });
});

/**
 * The rendered HTML once the enter fade has finished: no element still at its
 * initial `opacity: 0` (the motion can start a few frames after mount) and
 * identical across two frames (not mid-animation).
 */
async function settledHtml(container: HTMLElement): Promise<string> {
  const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  let previous = '';

  await vi.waitFor(
    async () => {
      const current = container.innerHTML;

      expect(current).not.toMatch(/opacity: 0;/);
      await frame();
      await frame();
      expect(container.innerHTML).toBe(current);
      previous = current;
    },
    { timeout: 5000, interval: 50 },
  );

  return previous;
}

describe('Bar click actions', () => {
  test.each([
    ['cpu', <Cpu key="cpu" />],
    ['clock', <Clock key="clock" />],
    ['battery', <Battery key="battery" />],
    ['wifi', <Wifi key="wifi" />],
    ['weather', <Weather key="weather" />],
  ] as const)('clicking %s invokes bar_action with only the enum value', async (action, node) => {
    const { screen } = await renderWithBarData(node, data);

    await screen.getByRole('button').click();

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('bar_action', { action });
  });

  test('every outcome, a rejection included, produces no extra UI and no unhandled error', async () => {
    const debug = vi.spyOn(console, 'debug').mockImplementation(() => {});

    for (const result of [
      { kind: 'busy' },
      { kind: 'uncertainAfterIssuance' },
      { kind: 'unauthorized' },
    ]) {
      vi.mocked(invoke).mockResolvedValueOnce(result);
      const { screen } = await renderWithBarData(<Cpu />, data);
      const before = await settledHtml(screen.container);

      await screen.getByRole('button').click();

      await vi.waitFor(() => expect(debug).toHaveBeenCalled());
      expect(screen.container.innerHTML).toBe(before);
      debug.mockClear();
      await screen.unmount();
    }

    vi.mocked(invoke).mockRejectedValueOnce(new Error('ipc down'));
    const { screen } = await renderWithBarData(<Cpu />, data);

    await screen.getByRole('button').click();

    await vi.waitFor(() => expect(debug).toHaveBeenCalled());
    debug.mockRestore();
  });

  test('a weather click outcome leaves the weather DOM unchanged', async () => {
    const debug = vi.spyOn(console, 'debug').mockImplementation(() => {});

    for (const result of [{ kind: 'busy' }, { kind: 'unauthorized' }, { kind: 'completed' }]) {
      vi.mocked(invoke).mockResolvedValueOnce(result);
      const { screen } = await renderWithBarData(<Weather />, data);
      const before = await settledHtml(screen.container);

      await screen.getByRole('button').click();

      await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
      expect(invoke).toHaveBeenLastCalledWith('bar_action', { action: 'weather' });
      expect(screen.container.innerHTML).toBe(before);
      vi.mocked(invoke).mockClear();
      await screen.unmount();
    }

    debug.mockRestore();
  });

  test.each([
    'com.spotify.client',
    'com.microsoft.edgemac.Dev',
    'com.tidal.desktop',
    'org.jeffvli.feishin',
    'com.example.unknown',
    '',
  ])('clicking media (%j) sends only "media" and no identifier', async (bundleIdentifier) => {
    const { screen } = await renderWithBarData(
      <Media />,
      barData({
        media: { title: 'T', artist: 'A', bundleIdentifier, playing: true },
      }),
    );

    await screen.getByRole('button').click();

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('bar_action', { action: 'media' });
  });

  test('every media click outcome, notAttempted and a rejection included, leaves the DOM unchanged', async () => {
    const debug = vi.spyOn(console, 'debug').mockImplementation(() => {});
    const media = barData({
      media: { title: 'T', artist: 'A', bundleIdentifier: 'com.example.unknown', playing: false },
    });

    for (const result of [
      { kind: 'notAttempted' },
      { kind: 'completed' },
      { kind: 'busy' },
      { kind: 'notInstalled' },
      new Error('ipc down'),
    ]) {
      if (result instanceof Error) {
        vi.mocked(invoke).mockRejectedValueOnce(result);
      } else {
        vi.mocked(invoke).mockResolvedValueOnce(result);
      }

      const { screen } = await renderWithBarData(<Media />, media);
      const before = await settledHtml(screen.container);

      await screen.getByRole('button').click();

      await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
      await new Promise((resolve) => setTimeout(resolve, 20));
      expect(invoke).toHaveBeenLastCalledWith('bar_action', { action: 'media' });
      expect(screen.container.innerHTML).toBe(before);
      vi.mocked(invoke).mockClear();
      await screen.unmount();
    }

    debug.mockRestore();
  });
});

describe('Keep Awake click', () => {
  const running = barData({
    keepAwake: true,
    keepAwakeSession: { paused: false, enabled: true },
  });
  const paused = barData({
    keepAwake: false,
    keepAwakeSession: { paused: true, enabled: true },
  });

  test.each([
    ['running', running, 'pause'],
    ['paused', paused, 'resume'],
    [
      'locked but running',
      barData({ keepAwake: false, keepAwakeSession: { paused: false, enabled: true } }),
      'pause',
    ],
  ] as const)(
    'clicking while %s sends only the fixed module and the observed action',
    async (_name, state, action) => {
      const { screen } = await renderWithBarData(<KeepAwake />, state);

      await screen.getByRole('button').click();

      expect(invoke).toHaveBeenCalledTimes(1);
      expect(invoke).toHaveBeenCalledWith('bar_session_action', { module: 'keepAwake', action });
    },
  );

  test('a disabled Keep Awake click is left to the host, which answers inert', async () => {
    vi.mocked(invoke).mockResolvedValue({ kind: 'disabled' });
    const { screen } = await renderWithBarData(
      <KeepAwake />,
      barData({ keepAwake: false, keepAwakeSession: { paused: false, enabled: false } }),
    );
    const before = await settledHtml(screen.container);

    await screen.getByRole('button').click();

    await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    expect(invoke).toHaveBeenCalledWith('bar_session_action', {
      module: 'keepAwake',
      action: 'pause',
    });
    expect(screen.container.innerHTML).toBe(before);
  });

  test('every outcome, a rejection included, leaves the DOM unchanged', async () => {
    const debug = vi.spyOn(console, 'debug').mockImplementation(() => {});

    for (const result of [
      { kind: 'applied' },
      { kind: 'notApplied', reason: 'refused' },
      { kind: 'stale' },
      { kind: 'disabled' },
      { kind: 'unavailable', reason: 'failed' },
      { kind: 'busy' },
      { kind: 'uncertain' },
      { kind: 'unauthorized' },
      new Error('ipc down'),
    ]) {
      if (result instanceof Error) {
        vi.mocked(invoke).mockRejectedValueOnce(result);
      } else {
        vi.mocked(invoke).mockResolvedValueOnce(result);
      }

      const { screen } = await renderWithBarData(<KeepAwake />, running);
      const before = await settledHtml(screen.container);

      await screen.getByRole('button').click();

      await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
      await new Promise((resolve) => setTimeout(resolve, 20));
      expect(screen.container.innerHTML).toBe(before);
      vi.mocked(invoke).mockClear();
      await screen.unmount();
    }

    debug.mockRestore();
  });

  test('an unresolved Keep Awake renders nothing and sends nothing', async () => {
    const { screen } = await renderWithBarData(<KeepAwake />, barData());

    expect(screen.container.querySelector('button')).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });
});
