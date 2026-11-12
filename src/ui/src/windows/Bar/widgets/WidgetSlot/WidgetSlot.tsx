import { Suspense, useState } from 'react';
import { ErrorBoundary } from 'react-error-boundary';

import type { BarLayoutItem } from '@/ipc';

import { lazyWidget, resolveWidget, useWidgetRegistry } from '../resolve';
import type { WidgetDefinition, WidgetOptions, WidgetZone } from '../types';

/** One published layout item (`id`, `widget`, `options`) and its zone. */
export type WidgetSlotProps = BarLayoutItem & { zone: WidgetZone };

/**
 * Holds one mount's lazy component. It sits outside `Suspense`, so it commits
 * while the widget loads and keeps the same component until it unmounts: a
 * rejected load is retried only by a later mount (see `lazyWidget`).
 */
function LoadedWidget({
  definition,
  id,
  zone,
  options,
}: {
  definition: WidgetDefinition;
  id: string;
  zone: WidgetZone;
  options: WidgetOptions;
}) {
  const [Widget] = useState(() => lazyWidget(definition));

  return (
    <Suspense fallback={null}>
      <Widget id={id} zone={zone} options={options} />
    </Suspense>
  );
}

/**
 * Resolves one layout item and renders its lazy widget inside its own
 * `Suspense` and error boundary. A widget that throws or fails to load renders
 * nothing and is logged once; an unregistered kind renders nothing. A new
 * `widget` kind for the same `id` remounts the boundary, so a failed slot
 * recovers and no state of the old kind leaks into the new one. New `options`
 * for the same kind re-render the same widget instance, keeping its state.
 */
export function WidgetSlot({ id, widget, zone, options }: WidgetSlotProps) {
  const definition = resolveWidget(useWidgetRegistry(), widget);

  if (!definition) {
    return null;
  }

  return (
    <ErrorBoundary
      key={widget}
      fallback={null}
      onError={(error) => {
        console.error(`[bar] widget "${id}" (${widget}) failed and renders nothing`, error);
      }}
    >
      <LoadedWidget definition={definition} id={id} zone={zone} options={options} />
    </ErrorBoundary>
  );
}
