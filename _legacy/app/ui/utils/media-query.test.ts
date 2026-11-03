import { describe, expect, test } from 'vitest';

import { LAPTOP_MEDIA_QUERY } from './media-query';

describe('media-query', () => {
  test('exports correct laptop media query', () => {
    expect(LAPTOP_MEDIA_QUERY).toBe('(width <= 2036px)');
  });
});
