import { describe, expect, it } from 'vitest';

import type { HostStatus, ModuleState, ModuleStatusView } from '@/ipc';

import {
  bannerTitle,
  enableOutcomeText,
  headerLine,
  moduleActions,
  moduleName,
  reconcile,
  sessionOutcomeText,
  STATE_LABELS,
} from './OverviewTab.model';

const module = (
  state: ModuleState,
  enabled: boolean,
  paused: boolean,
  id = 'testModule',
): ModuleStatusView => ({
  id,
  state,
  enabled,
  paused,
  revision: '1',
  detail: null,
  diagnostic: null,
});

const status = (overrides: Partial<HostStatus> = {}): HostStatus => ({
  ready: true,
  error: null,
  access: { state: 'open', revoked: false, appPath: null },
  modules: [],
  config: [],
  ...overrides,
});

describe('OverviewTab model', () => {
  it('names every module readably', () => {
    expect(
      [
        'commandQuit',
        'keepAwake',
        'menuAnywhere',
        'notunes',
        'proxyAudio',
        'bar',
        'tiling',
        'keybindings',
        'wallpapers',
      ].map(moduleName),
    ).toEqual([
      'Hold to Quit',
      'Keep Awake',
      'Menu Anywhere',
      'noTunes',
      'Proxy Audio',
      'Bar',
      'Tiling',
      'Keybindings',
      'Wallpapers',
    ]);
  });

  it('keeps the former tray state labels', () => {
    expect(STATE_LABELS).toEqual({
      stopped: 'stopped',
      disabled: 'disabled',
      running: 'running',
      paused: 'paused',
      waitingForPermission: 'waiting for permission',
      unavailable: 'unavailable',
      failed: 'failed',
    });
  });

  // The former tray's `module_rows`: Pause/Resume for an enabled module,
  // Retry for every failed one and an enabled, unpaused unavailable one.
  const STATES: ModuleState[] = [
    'stopped',
    'disabled',
    'running',
    'paused',
    'waitingForPermission',
    'unavailable',
    'failed',
  ];

  for (const state of STATES) {
    for (const enabled of [false, true]) {
      for (const paused of [false, true]) {
        it(`eligibility: ${state}, enabled ${enabled}, paused ${paused}`, () => {
          const session = enabled ? (paused ? 'resume' : 'pause') : null;
          const retry = state === 'failed' || (state === 'unavailable' && enabled && !paused);

          expect(moduleActions(module(state, enabled, paused))).toEqual({ session, retry });
        });
      }
    }
  }

  it('reads the header line and the banner title', () => {
    expect(headerLine(status({ ready: false }))).toBe('starting…');
    expect(
      headerLine(status({ access: { state: 'checking', revoked: false, appPath: null } })),
    ).toBe('checking Accessibility…');
    expect(headerLine(status())).toBeNull();
    expect(bannerTitle({ state: 'blocked', revoked: false, appPath: null })).toBe(
      'Accessibility required',
    );
    expect(bannerTitle({ state: 'blocked', revoked: true, appPath: null })).toBe(
      'Accessibility was turned off',
    );
    expect(bannerTitle({ state: 'checking', revoked: false, appPath: null })).toBeNull();
    expect(bannerTitle({ state: 'open', revoked: true, appPath: null })).toBeNull();
  });

  it('shows no text for applied or stale session outcomes', () => {
    expect(sessionOutcomeText('pause', { kind: 'applied' })).toBeNull();
    expect(sessionOutcomeText('resume', { kind: 'stale' })).toBeNull();
    expect(sessionOutcomeText('pause', { kind: 'busy' })).toBe(
      'Could not pause: another action is in progress',
    );
    expect(sessionOutcomeText('resume', { kind: 'unavailable', reason: 'releasePending' })).toBe(
      'Could not resume: a release is pending',
    );
  });

  it('keeps enable pending for its outcome, whatever statuses arrive', () => {
    const pending = { keepAwake: { kind: 'enable' as const, target: true } };
    const reflected = status({ modules: [module('running', true, false, 'keepAwake')] });
    const refused = status({ modules: reflected.modules, error: 'cannot write keepAwake.toml' });

    expect(reconcile(pending, {}, reflected).pending).toBe(pending);
    expect(reconcile(pending, {}, refused).pending).toBe(pending);
  });

  it('shows the host message for a refused or unavailable enable only', () => {
    expect(enableOutcomeText({ kind: 'applied' })).toBeNull();
    expect(enableOutcomeText({ kind: 'unchanged' })).toBeNull();
    expect(enableOutcomeText({ kind: 'refused', message: 'keepAwake.toml is invalid' })).toBe(
      'keepAwake.toml is invalid',
    );
    expect(enableOutcomeText({ kind: 'unavailable', message: 'the host is shutting down' })).toBe(
      'the host is shutting down',
    );
  });

  it('settles retry on the next status and keeps session actions for their outcome', () => {
    const before = status({ modules: [module('failed', true, false, 'proxyAudio')] });
    const pending = {
      proxyAudio: { kind: 'retry' as const, baseline: before },
      tiling: { kind: 'session' as const, action: 'pause' as const },
    };
    const after = status({ modules: before.modules, error: 'start failed again' });

    expect(reconcile(pending, {}, after)).toEqual({
      pending: { tiling: pending.tiling },
      errors: { proxyAudio: 'start failed again' },
    });
  });
});
