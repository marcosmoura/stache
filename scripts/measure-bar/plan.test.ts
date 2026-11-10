import { describe, expect, it } from 'vitest';

import { armOrder, pairOf, renderBarToml, seriesEnvironment } from './plan';

describe('armOrder', () => {
  it('alternates which arm goes first in every pair', () => {
    expect(armOrder(4)).toEqual(['off', 'on', 'on', 'off', 'off', 'on', 'on', 'off']);
  });

  it('runs every arm exactly launchesPerArm times', () => {
    const order = armOrder(5);

    expect(order.filter((arm) => arm === 'off')).toHaveLength(5);
    expect(order.filter((arm) => arm === 'on')).toHaveLength(5);
    expect(order.map((_, index) => pairOf(index))).toEqual([0, 0, 1, 1, 2, 2, 3, 3, 4, 4]);
  });
});

describe('trial configuration', () => {
  it('differs between the arms only in enabled', () => {
    expect(renderBarToml('off', 28, 12)).toBe('enabled = false\nheight = 28\npadding = 12\n');
    expect(renderBarToml('on', 28, 12)).toBe('enabled = true\nheight = 28\npadding = 12\n');
  });

  it('adds the SSID switch only for the off series', () => {
    expect(seriesEnvironment('default')).toEqual({});
    expect(seriesEnvironment('off')).toEqual({ STACHE_BAR_WIFI_SSID_SUMMARY: '0' });
  });
});
