import type { PartialWidgetRegistry } from '@/windows/Bar/widgets';

import { ActionWidget, CounterWidget, LabelWidget, ThrowingWidget } from './widgets';

/**
 * The fixture registry: `clock` and `cpu` are counters, `spaces` and
 * `weather` are labels, `media` is a `BarItem` sending `bar_action`, `wifi` throws, `battery` fails to load and
 * `keep_awake` is not registered.
 */
export const fixtureRegistry: PartialWidgetRegistry = {
  clock: { load: () => Promise.resolve({ default: CounterWidget }) },
  cpu: { load: () => Promise.resolve({ default: CounterWidget }) },
  spaces: { load: () => Promise.resolve({ default: LabelWidget }) },
  media: { load: () => Promise.resolve({ default: ActionWidget }) },
  weather: { load: () => Promise.resolve({ default: LabelWidget }) },
  wifi: { load: () => Promise.resolve({ default: ThrowingWidget }) },
  battery: { load: () => Promise.reject(new Error('fixture widget battery failed to load')) },
};
