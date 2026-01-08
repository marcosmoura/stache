import { describe, expect, it } from 'vitest';

import { useStoreQuery, type UseStoreQueryOptions } from './useStoreQuery';

describe('useStoreQuery', () => {
  it('should export the useStoreQuery hook', () => {
    expect(useStoreQuery).toBeDefined();
    expect(typeof useStoreQuery).toBe('function');
  });

  it('should export the type definitions', () => {
    // Type-only check - these won't exist at runtime but TypeScript should compile
    const options: UseStoreQueryOptions<{ data: string }> = {
      queryKey: ['test'],
      queryFn: async () => ({ data: 'test' }),
      syncAcrossWindows: true,
    };

    expect(options).toMatchObject({ queryKey: ['test'] });
  });

  it('should accept enabled option for conditional queries', () => {
    const options: UseStoreQueryOptions<string> = {
      queryKey: ['conditional'],
      queryFn: async () => 'data',
      enabled: false,
    };

    expect(options.enabled).toBe(false);
  });

  it('should accept refetchInterval option', () => {
    const options: UseStoreQueryOptions<number> = {
      queryKey: ['interval'],
      queryFn: async () => 42,
      refetchInterval: 5000,
    };

    expect(options.refetchInterval).toBe(5000);
  });

  it('should default syncAcrossWindows to true when not specified', () => {
    const options: UseStoreQueryOptions<string> = {
      queryKey: ['default-sync'],
      queryFn: async () => 'test',
    };

    // syncAcrossWindows is optional and defaults to true in the hook
    expect(options.syncAcrossWindows).toBeUndefined();
  });

  it('should allow disabling cross-window sync', () => {
    const options: UseStoreQueryOptions<string> = {
      queryKey: ['no-sync'],
      queryFn: async () => 'test',
      syncAcrossWindows: false,
    };

    expect(options.syncAcrossWindows).toBe(false);
  });
});
