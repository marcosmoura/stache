import { describe, expect, it } from 'vitest';

import { errorMessage } from './errorMessage';

describe('errorMessage', () => {
  it('reads an Error message and stringifies anything else', () => {
    expect(errorMessage(new Error('boom'))).toBe('boom');
    expect(errorMessage('denied')).toBe('denied');
    expect(errorMessage(42)).toBe('42');
  });
});
