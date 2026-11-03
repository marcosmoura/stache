import { describe, expect, it } from 'vitest';

import {
  createStore,
  getStore,
  getStoreIds,
  type CreateStoreOptions,
  type State,
} from './createStore';

describe('createStore', () => {
  it('should export the createStore function', () => {
    expect(createStore).toBeDefined();
    expect(typeof createStore).toBe('function');
  });

  it('should export the getStore function', () => {
    expect(getStore).toBeDefined();
    expect(typeof getStore).toBe('function');
  });

  it('should export the getStoreIds function', () => {
    expect(getStoreIds).toBeDefined();
    expect(typeof getStoreIds).toBe('function');
  });

  it('should export the type definitions', () => {
    // Type-only check - these won't exist at runtime but TypeScript should compile
    const options: CreateStoreOptions = {
      autoStart: true,
      syncStrategy: 'debounce',
      syncInterval: 100,
      save: false,
      filterKeys: [],
    };

    const state: State = { key: 'value' };

    // If this compiles, the types are exported correctly
    expect(options).toMatchObject({ autoStart: true });
    expect(state).toHaveProperty('key');
  });
});
