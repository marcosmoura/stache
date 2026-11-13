import { afterEach, describe, expect, it, vi } from 'vitest';

import { renderWithSnapshot } from '../../../../../tests/barSnapshot';
import { barAvailable, barUnavailable } from '../../../../../tests/fixtures';
import { mockHost } from '../../../../../tests/ipc';

import { KeepAwake } from './KeepAwake';
import { keepAwakeAction, keepAwakeLabel, selectKeepAwake } from './KeepAwake.state';

/* Catppuccin green (Mocha, Latte). */
const SUCCESS = /rgb\(166, 227, 161\)|rgb\(64, 160, 43\)/;

const icon = () => getComputedStyle(document.querySelector('button svg')!);

afterEach(() => {
  vi.restoreAllMocks();
});

describe('Keep Awake state', () => {
  it('derives the action from paused and the name from active', () => {
    expect(keepAwakeAction({ active: true, paused: false, enabled: true })).toBe('pause');
    expect(keepAwakeAction({ active: false, paused: false, enabled: true })).toBe('pause');
    expect(keepAwakeAction({ active: false, paused: true, enabled: true })).toBe('resume');
    expect(keepAwakeLabel({ active: true, paused: false, enabled: true })).toBe('Keep Awake: on');
    expect(keepAwakeLabel({ active: false, paused: true, enabled: true })).toBe('Keep Awake: off');
  });

  it('selects the indicator only while available', () => {
    expect(selectKeepAwake(barAvailable('1'))).toBeNull();
    expect(selectKeepAwake(barUnavailable('1'))).toBeNull();
  });
});

describe('Keep Awake', () => {
  it('is hidden until the first observation', async () => {
    const { screen } = await renderWithSnapshot(
      <KeepAwake id="keep_awake" zone="end" options={{}} />,
      barAvailable('1', { keepAwake: null }),
    );

    await new Promise((done) => setTimeout(done, 20));
    expect(screen.container.childElementCount).toBe(0);
  });

  it('is filled in the success color while awake, outlined otherwise', async () => {
    const { screen, setView } = await renderWithSnapshot(
      <KeepAwake id="keep_awake" zone="end" options={{}} />,
      barAvailable('1', { keepAwake: { active: true, paused: false, enabled: true } }),
    );

    await expect.element(screen.getByRole('button', { name: 'Keep Awake: on' })).toBeVisible();
    await vi.waitFor(() => expect(icon().color).toMatch(SUCCESS));
    await vi.waitFor(() => expect(icon().fill).not.toMatch(/transparent|none|rgba\(0, 0, 0, 0\)/));

    setView({ keepAwake: { active: false, paused: false, enabled: true } });
    await expect.element(screen.getByRole('button', { name: 'Keep Awake: off' })).toBeVisible();
    await vi.waitFor(() => expect(icon().color).not.toMatch(SUCCESS));
    await vi.waitFor(() => expect(icon().fill).toMatch(/transparent|rgba\(0, 0, 0, 0\)/));
  });

  it('sends pause when not paused and resume when paused', async () => {
    const host = mockHost().answer('bar_session_action', { kind: 'applied' });
    const { screen, setView } = await renderWithSnapshot(
      <KeepAwake id="keep_awake" zone="end" options={{}} />,
      barAvailable('1', { keepAwake: { active: true, paused: false, enabled: true } }),
    );

    await screen.getByRole('button').click();
    await vi.waitFor(() => expect(host.callsOf('bar_session_action')).toHaveLength(1));

    setView({ keepAwake: { active: false, paused: true, enabled: true } });
    await expect.element(screen.getByRole('button', { name: 'Keep Awake: off' })).toBeVisible();
    await screen.getByRole('button').click();
    await vi.waitFor(() => expect(host.callsOf('bar_session_action')).toHaveLength(2));

    expect(host.callsOf('bar_session_action').map((call) => call.args)).toEqual([
      { module: 'keepAwake', action: 'pause' },
      { module: 'keepAwake', action: 'resume' },
    ]);
  });
});
