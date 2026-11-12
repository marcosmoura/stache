export type {
  WidgetComponent,
  WidgetDefinition,
  WidgetOptions,
  PartialWidgetRegistry,
  WidgetProps,
  WidgetRegistry,
  WidgetZone,
} from './types';
export { productionRegistry } from './registry';
export { lazyWidget, resolveWidget, useWidgetRegistry } from './resolve';
export { WidgetSlot } from './WidgetSlot';
