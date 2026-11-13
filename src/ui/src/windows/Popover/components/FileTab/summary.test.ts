import { describe, expect, it } from 'vitest';

import { summary } from './summary';

describe('summary', () => {
  it('keeps only the first sentence', () => {
    expect(
      summary(
        'Whether Stache draws borders around Tiling-managed windows. Borders are drawn by Stache itself.',
      ),
    ).toBe('Whether Stache draws borders around Tiling-managed windows.');
  });

  it('does not split inside a value such as 0.5 or a file name', () => {
    expect(summary('Ratio between 0.1 and 0.9. Larger values grow the master.')).toBe(
      'Ratio between 0.1 and 0.9.',
    );
    expect(summary('Path to config.toml relative to home. Ignored when absent.')).toBe(
      'Path to config.toml relative to home.',
    );
  });

  it('keeps only the first line', () => {
    expect(summary('First line without a stop\nsecond line.')).toBe('First line without a stop');
  });

  it('returns a single sentence unchanged', () => {
    expect(summary('  Width in pixels.  ')).toBe('Width in pixels.');
  });
});
