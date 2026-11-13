import { lazy, use, type LazyExoticComponent } from 'react';

import type { BarWidgetKind } from '@/ipc';

import { WidgetRegistryContext } from './registry';
import type { PartialWidgetRegistry, WidgetComponent, WidgetDefinition } from './types';

/** The registry in scope. */
export function useWidgetRegistry(): PartialWidgetRegistry {
  return use(WidgetRegistryContext);
}

const warned = new Set<BarWidgetKind>();

/**
 * The definition registered for `kind`, or `null`. In development an
 * unregistered kind warns once per kind.
 *
 * The result is widened to accept any kind's options: the registry is keyed
 * by kind, so the caller hands it only the items of `kind`.
 */
export function resolveWidget(
  registry: PartialWidgetRegistry,
  kind: BarWidgetKind,
): WidgetDefinition | null {
  const definition = registry[kind] as WidgetDefinition | undefined;

  if (definition) {
    return definition;
  }

  if (import.meta.env.DEV && !warned.has(kind)) {
    warned.add(kind);
    console.warn(`[bar] no widget is registered for "${kind}"; its items render nothing`);
  }

  return null;
}

const lazyComponents = new WeakMap<WidgetDefinition, LazyExoticComponent<WidgetComponent>>();

/**
 * The lazy component of a definition, created once per definition. A rejected
 * `load` evicts it, so the next mount retries instead of staying blank until
 * reload.
 */
export function lazyWidget(definition: WidgetDefinition) {
  let component = lazyComponents.get(definition);

  if (!component) {
    const created = lazy(() =>
      definition.load().catch((error: unknown) => {
        if (lazyComponents.get(definition) === created) {
          lazyComponents.delete(definition);
        }

        throw error;
      }),
    );

    lazyComponents.set(definition, created);
    component = created;
  }

  return component;
}
