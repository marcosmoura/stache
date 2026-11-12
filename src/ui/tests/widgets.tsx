import { useState } from 'react';

import { useBarAction } from '@/queries/barActions';
import { BarItem } from '@/windows/Bar/components/BarItem';
import type { WidgetProps } from '@/windows/Bar/widgets';

/*
 * Fixture widgets for component tests and the end-to-end harness. Plain
 * inline styles: these are test fixtures, not product components.
 */

/** Stateful: a button counting its own clicks; `data-options` shows its options. */
export function CounterWidget({ id, zone, options }: WidgetProps) {
  const [count, setCount] = useState(0);

  return (
    <button
      type="button"
      data-testid={`widget-${id}`}
      data-zone={zone}
      data-options={JSON.stringify(options)}
      onClick={() => setCount((value) => value + 1)}
    >
      {id}:{count}
    </button>
  );
}

/** A label of a fixed width, so zone widths can differ. */
export function LabelWidget({ id, zone }: WidgetProps) {
  return (
    <span
      data-testid={`widget-${id}`}
      data-zone={zone}
      style={{ display: 'inline-block', width: id.startsWith('wide') ? 240 : 40 }}
    >
      {id}
    </span>
  );
}

/** Throws while rendering. */
export function ThrowingWidget({ id }: WidgetProps): never {
  throw new Error(`fixture widget ${id} throws`);
}

/** A clickable `BarItem` that sends `bar_action("media")`, as a real widget would. */
export function ActionWidget({ id, zone }: WidgetProps) {
  const run = useBarAction();

  return (
    <span data-testid={`widget-${id}`} data-zone={zone}>
      <BarItem onClick={() => run('media')}>{id}</BarItem>
    </span>
  );
}
