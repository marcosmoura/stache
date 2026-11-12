import { describe, expect, it } from 'vitest';

import { durations, easings, transitions } from './motion';
import { duration, easing } from './tokens.stylex';

describe('motion values', () => {
  it('durations agree with the StyleX duration consts', () => {
    expect(Object.keys(duration).sort()).toEqual(Object.keys(durations).sort());

    for (const [name, seconds] of Object.entries(durations)) {
      expect(duration[name as keyof typeof duration]).toBe(`${Math.round(seconds * 1000)}ms`);
    }
  });

  it('easings agree with the StyleX easing consts', () => {
    expect(Object.keys(easing).sort()).toEqual(Object.keys(easings).sort());

    for (const [name, curve] of Object.entries(easings)) {
      expect(easing[name as keyof typeof easing]).toBe(`cubic-bezier(${curve.join(', ')})`);
    }
  });

  it('presets use only the named values', () => {
    const named = new Set<unknown>(Object.values(durations));
    const curves = new Set<unknown>(Object.values(easings));

    for (const preset of Object.values(transitions)) {
      expect(named.has(preset.duration)).toBe(true);

      if ('ease' in preset) {
        expect(curves.has(preset.ease)).toBe(true);
      }
    }
  });

  it("reproduce today's Bar motion", () => {
    expect(easings.inOut).toEqual([0.42, 0, 0.58, 1]);
    expect(durations.bar).toBe(0.15);
    expect(transitions.fadeIn).toEqual({ duration: 0.275, ease: [0.42, 0, 0.58, 1] });
    expect(transitions.spring).toEqual({ type: 'spring', bounce: 0, duration: 0.5 });
  });
});
