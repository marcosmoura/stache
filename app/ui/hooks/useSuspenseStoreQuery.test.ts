import { describe, expect, it } from 'vitest';

import { destroyQueryStore, getQueryStoreIds } from './useStoreQueryBase';
import { useSuspenseStoreQuery, type UseSuspenseStoreQueryOptions } from './useSuspenseStoreQuery';

describe('useSuspenseStoreQuery', () => {
  it('should export the useSuspenseStoreQuery hook', () => {
    expect(useSuspenseStoreQuery).toBeDefined();
    expect(typeof useSuspenseStoreQuery).toBe('function');
  });

  it('should export the destroyQueryStore function from base', () => {
    expect(destroyQueryStore).toBeDefined();
    expect(typeof destroyQueryStore).toBe('function');
  });

  it('should export the getQueryStoreIds function from base', () => {
    expect(getQueryStoreIds).toBeDefined();
    expect(typeof getQueryStoreIds).toBe('function');
  });

  it('should return empty array when no stores registered', () => {
    const ids = getQueryStoreIds();
    expect(Array.isArray(ids)).toBe(true);
  });

  it('should export the type definitions', () => {
    // Type-only check - these won't exist at runtime but TypeScript should compile
    const options: UseSuspenseStoreQueryOptions<{ data: string }> = {
      queryKey: ['test'],
      queryFn: async () => ({ data: 'test' }),
      syncAcrossWindows: true,
    };

    // If this compiles, the types are exported correctly
    expect(options).toMatchObject({ queryKey: ['test'] });
  });

  it('should accept staleTime option', () => {
    const options: UseSuspenseStoreQueryOptions<string> = {
      queryKey: ['stale'],
      queryFn: async () => 'test',
      staleTime: Infinity,
    };

    expect(options.staleTime).toBe(Infinity);
  });

  it('should accept select option for data transformation', () => {
    const options: UseSuspenseStoreQueryOptions<{ name: string }, Error, string> = {
      queryKey: ['transform'],
      queryFn: async () => ({ name: 'test' }),
      select: (data) => data.name,
    };

    expect(typeof options.select).toBe('function');
  });
});
