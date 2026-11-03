/**
 * Store system using Zustand + Tauri Store + Immer
 *
 * This module provides a simplified API for creating cross-window synchronized stores
 * with Immer-powered state updates.
 *
 * @example
 * ```tsx
 * // Create a store with Immer mutations
 * const useBatteryState = createStore('battery', (set) => ({
 *   value1: 0,
 *   value2: 0,
 *   updateValue1: () => set((state) => { state.value1++ }),
 *   setValue2: (value: number) => set((state) => { state.value2 = value }),
 * }));
 *
 * // Use in components with selectors
 * const MyComponent = () => {
 *   const value1 = useBatteryState((state) => state.value1);
 *   const updateValue1 = useBatteryState((state) => state.updateValue1);
 *   return <button onClick={updateValue1}>{value1}</button>;
 * };
 * ```
 */
import { createTauriStore, type TauriStore } from '@tauri-store/zustand';
import { create, type StateCreator, type StoreApi, type UseBoundStore } from 'zustand';
import { immer } from 'zustand/middleware/immer';

/** Generic state type constraint */
export type State = Record<string, unknown>;

/** Immer-based state creator that allows direct mutations */
type ImmerStateCreator<T extends State> = StateCreator<T, [['zustand/immer', never]], [], T>;

/** Options for store creation */
export interface CreateStoreOptions {
  /** Whether to automatically start synchronization (default: true) */
  autoStart?: boolean;
  /** Synchronization strategy (default: 'debounce') */
  syncStrategy?: 'immediate' | 'debounce' | 'throttle';
  /** Sync interval in ms when using debounce/throttle (default: 100) */
  syncInterval?: number;
  /** Whether to persist to disk (default: false) */
  save?: boolean;
  /** Keys to exclude from synchronization */
  filterKeys?: string[];
}

/** Result of createStore containing the hook and Tauri handler */
export interface StoreResult<T extends State> {
  /** React hook to access and subscribe to the store */
  useStore: UseBoundStore<StoreApi<T>>;
  /** The underlying Tauri store handler for advanced operations */
  tauriStore: TauriStore<T, StoreApi<T>>;
  /** Start the store synchronization manually (if autoStart is false) */
  start: () => Promise<void>;
  /** Stop the store synchronization */
  stop: () => Promise<void>;
  /** Destroy the store and cleanup resources */
  destroy: () => Promise<void>;
}

// Registry to track all created stores for debugging and cleanup
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const storeRegistry = new Map<string, StoreResult<any>>();

/**
 * Creates a Zustand store with Tauri synchronization and Immer-powered mutations.
 *
 * The store automatically syncs state across multiple windows using
 * tauri-plugin-zustand. State updates use Immer for immutable updates
 * with a mutable API.
 *
 * Store ID is automatically suffixed with '-store' internally.
 *
 * @param storeId - Unique identifier for the store (will be suffixed with '-store')
 * @param initializer - State creator function with Immer mutations support
 * @param options - Store configuration options
 * @returns React hook bound to the store with full Zustand selector support
 *
 * @example
 * ```tsx
 * // Simple counter store
 * const useCounter = createStore('counter', (set) => ({
 *   count: 0,
 *   increment: () => set((state) => { state.count++ }),
 *   decrement: () => set((state) => { state.count-- }),
 *   reset: () => set((state) => { state.count = 0 }),
 * }));
 *
 * // With async actions
 * const useUser = createStore('user', (set) => ({
 *   user: null,
 *   loading: false,
 *   fetchUser: async (id: string) => {
 *     set((state) => { state.loading = true });
 *     const user = await fetchUserById(id);
 *     set((state) => {
 *       state.user = user;
 *       state.loading = false;
 *     });
 *   },
 * }));
 * ```
 */
export function createStore<T extends State>(
  storeId: string,
  initializer: ImmerStateCreator<T>,
  options: CreateStoreOptions = {},
): UseBoundStore<StoreApi<T>> {
  const fullStoreId = `${storeId}-store`;

  // Return existing store if already created
  if (storeRegistry.has(fullStoreId)) {
    return storeRegistry.get(fullStoreId)!.useStore as UseBoundStore<StoreApi<T>>;
  }

  const {
    autoStart = true,
    syncStrategy = 'debounce',
    syncInterval = 100,
    save = false,
    filterKeys,
  } = options;

  // Create Zustand store with Immer middleware
  const useStore = create<T>()(immer(initializer));

  // Create Tauri store handler for cross-window sync
  // Use type assertion because Immer middleware changes the store type
  const tauriStore = createTauriStore(fullStoreId, useStore as unknown as StoreApi<T>, {
    autoStart,
    syncStrategy,
    syncInterval,
    save,
    filterKeys: filterKeys ?? [],
    filterKeysStrategy: 'omit',
  }) as unknown as TauriStore<T, StoreApi<T>>;

  // Create result object
  const result: StoreResult<T> = {
    useStore: useStore as unknown as UseBoundStore<StoreApi<T>>,
    tauriStore,
    start: () => tauriStore.start(),
    stop: () => tauriStore.stop(),
    destroy: () => tauriStore.destroy(),
  };

  storeRegistry.set(fullStoreId, result);

  return useStore as unknown as UseBoundStore<StoreApi<T>>;
}

/**
 * Gets the full store result including Tauri handler for advanced operations.
 *
 * @param storeId - The store identifier (without '-store' suffix)
 * @returns The full store result or undefined if not found
 */
export function getStore<T extends State>(storeId: string): StoreResult<T> | undefined {
  return storeRegistry.get(`${storeId}-store`) as StoreResult<T> | undefined;
}

/**
 * Gets all registered store IDs.
 */
export function getStoreIds(): string[] {
  return Array.from(storeRegistry.keys());
}

/**
 * Destroys a store and removes it from the registry.
 *
 * @param storeId - The store identifier (without '-store' suffix)
 */
export async function destroyStore(storeId: string): Promise<void> {
  const fullStoreId = `${storeId}-store`;
  const store = storeRegistry.get(fullStoreId);

  if (store) {
    await store.destroy();
    storeRegistry.delete(fullStoreId);
  }
}
