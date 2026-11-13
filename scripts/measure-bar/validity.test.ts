import { describe, expect, it } from 'vitest';

import type { HostWindow } from './types';
import { judgeTrial, parseCoreStatusAccess, parseHostWindows } from './validity';

const APP = '/r/Stache.app';
const panel: HostWindow = { layer: 0, x: 0, y: 0, width: 1440, height: 28, onScreen: true };

function line(access: unknown): string {
  return JSON.stringify({ status: 'ok', data: { directory: '/d', files: [], access } });
}

describe('parseCoreStatusAccess', () => {
  it('reads the three gate states', () => {
    for (const state of ['open', 'blocked', 'checking'] as const) {
      expect(parseCoreStatusAccess(line({ state, revoked: false }))).toBe(state);
    }
  });

  it('reports a host without the field as unreported and garbage as null', () => {
    expect(parseCoreStatusAccess(JSON.stringify({ status: 'ok', data: { files: [] } }))).toBe(
      'unreported',
    );
    expect(parseCoreStatusAccess('not json')).toBeNull();
    expect(parseCoreStatusAccess(JSON.stringify({ status: 'error' }))).toBeNull();
  });
});

describe('parseHostWindows', () => {
  it('parses the helper output and rejects malformed entries', () => {
    expect(
      parseHostWindows('[{"layer":3,"x":0,"y":0,"width":10,"height":28,"onScreen":true}]'),
    ).toEqual([{ layer: 3, x: 0, y: 0, width: 10, height: 28, onScreen: true }]);
    expect(() => parseHostWindows('{}')).toThrow();
    expect(() => parseHostWindows('[{"layer":"x"}]')).toThrow();
  });
});

describe('judgeTrial', () => {
  const base = { appPath: APP, height: 28 };

  it('accepts a Bar-on launch with an open gate and a Bar-height window', () => {
    expect(judgeTrial({ ...base, arm: 'on', access: 'open', windows: [panel] })).toEqual({
      valid: true,
    });
  });

  it('rejects a gated launch in either arm and names the exact .app to trust', () => {
    for (const arm of ['on', 'off'] as const) {
      const verdict = judgeTrial({ ...base, arm, access: 'blocked', windows: [] });

      expect(verdict).toMatchObject({ valid: false, reason: 'access-blocked' });
      expect(verdict.valid ? '' : verdict.message).toContain(APP);
      expect(verdict.valid ? '' : verdict.message).toContain('Accessibility');
    }
  });

  it('rejects an unreported gate and a checking gate', () => {
    expect(
      judgeTrial({ ...base, arm: 'on', access: 'unreported', windows: [panel] }),
    ).toMatchObject({
      reason: 'access-unreported',
    });
    expect(judgeTrial({ ...base, arm: 'on', access: null, windows: [panel] })).toMatchObject({
      reason: 'access-unreported',
    });
    expect(judgeTrial({ ...base, arm: 'on', access: 'checking', windows: [panel] })).toMatchObject({
      reason: 'access-blocked',
    });
  });

  it('rejects a Bar-on launch without a Bar-height window', () => {
    const other: HostWindow = { ...panel, height: 400 };

    expect(judgeTrial({ ...base, arm: 'on', access: 'open', windows: [] })).toMatchObject({
      reason: 'bar-panel-missing',
    });
    expect(judgeTrial({ ...base, arm: 'on', access: 'open', windows: [other] })).toMatchObject({
      reason: 'bar-panel-missing',
    });
  });

  it('requires a Bar-off launch to own no window', () => {
    expect(judgeTrial({ ...base, arm: 'off', access: 'open', windows: [] })).toEqual({
      valid: true,
    });
    expect(judgeTrial({ ...base, arm: 'off', access: 'open', windows: [panel] })).toMatchObject({
      reason: 'unexpected-window',
    });
  });
});
