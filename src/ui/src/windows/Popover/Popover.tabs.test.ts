import { describe, expect, it } from 'vitest';

import type { ConfigFileView } from '@/ipc';

import { badgeOf, tabsFor } from './Popover.tabs';

const file = (name: string, overrides: Partial<ConfigFileView> = {}): ConfigFileView => ({
  file: name,
  path: `/config/${name}`,
  state: 'valid',
  active: true,
  diagnostic: null,
  warnings: [],
  ...overrides,
});

describe('Popover tabs', () => {
  it('puts Overview first, then the files in the order given', () => {
    const tabs = tabsFor([file('global.toml'), file('bar.toml'), file('unknown.toml')]);

    expect(tabs.map((tab) => tab.id)).toEqual([
      'overview',
      'global.toml',
      'bar.toml',
      'unknown.toml',
    ]);
    expect(tabs[0]?.label).toBe('Overview');
  });

  it('badges invalid bytes over a diagnostic over warnings', () => {
    expect(badgeOf(file('a', { state: 'invalid', diagnostic: 'x', warnings: ['w'] }))).toBe(
      'invalid',
    );
    expect(badgeOf(file('a', { diagnostic: 'x', warnings: ['w'] }))).toBe('diagnostic');
    expect(badgeOf(file('a', { warnings: ['w'] }))).toBe('warnings');
    expect(badgeOf(file('a', { state: 'missing' }))).toBeNull();
  });
});
