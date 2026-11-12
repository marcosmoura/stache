import { createContext } from 'react';

import type { PartialWidgetRegistry, WidgetRegistry } from './types';

/**
 * The production registry. Each definition is a dynamic import, so each
 * widget is its own chunk, loaded only when its kind is in the layout. The
 * type is exhaustive, so every kind must be registered.
 */
export const productionRegistry: WidgetRegistry = {
  battery: { load: () => import('./Battery') },
  clock: { load: () => import('./Clock') },
  cpu: { load: () => import('./Cpu') },
  keep_awake: { load: () => import('./KeepAwake') },
  media: { load: () => import('./Media') },
  spaces: { load: () => import('./Spaces') },
  weather: { load: () => import('./Weather') },
  wifi: { load: () => import('./Wifi') },
};

/** The registry the layout engine resolves widgets from; defaults to production. */
export const WidgetRegistryContext = createContext<PartialWidgetRegistry>(productionRegistry);
