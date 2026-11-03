import { describe, expect, it } from 'vitest';

import { uuid } from './uuid';

describe('uuid', () => {
  it('should return a string', () => {
    const result = uuid();
    expect(typeof result).toBe('string');
  });

  it('should return a valid UUID v4 format', () => {
    const result = uuid();
    const uuidV4Regex = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
    expect(result).toMatch(uuidV4Regex);
  });

  it('should always have "4" as the version digit', () => {
    for (let i = 0; i < 100; i++) {
      const result = uuid();
      expect(result[14]).toBe('4');
    }
  });

  it('should always have a valid variant digit (8, 9, a, or b)', () => {
    for (let i = 0; i < 100; i++) {
      const result = uuid();
      expect(['8', '9', 'a', 'b']).toContain(result[19]);
    }
  });

  it('should return unique values on each call', () => {
    const results = new Set<string>();
    const iterations = 1000;

    for (let i = 0; i < iterations; i++) {
      results.add(uuid());
    }

    expect(results.size).toBe(iterations);
  });

  it('should have the correct length of 36 characters', () => {
    const result = uuid();
    expect(result.length).toBe(36);
  });

  it('should have hyphens at the correct positions', () => {
    const result = uuid();
    expect(result[8]).toBe('-');
    expect(result[13]).toBe('-');
    expect(result[18]).toBe('-');
    expect(result[23]).toBe('-');
  });
});
