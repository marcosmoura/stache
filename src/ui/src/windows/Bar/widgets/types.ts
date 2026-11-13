import type { ComponentType } from 'react';

import type { BarLayoutItem, BarWidgetKind } from '@/ipc';

/** The Bar zone an item is placed in. */
export type WidgetZone = 'start' | 'center' | 'end';

/**
 * The resolved options of widget kind `K`, taken from the published layout
 * item (`bar.toml` entry options with defaults applied). An empty object for a
 * widget without options.
 */
export type WidgetOptions<K extends BarWidgetKind = BarWidgetKind> = OptionsByKind[K];

type OptionsByKind = { [Item in BarLayoutItem as Item['widget']]: Item['options'] };

/** Props a Bar widget of kind `K` receives. */
export type WidgetProps<K extends BarWidgetKind = BarWidgetKind> = {
  /** The item's unique, stable id from the normalized layout. */
  id: string;
  /** The zone the item is currently placed in. */
  zone: WidgetZone;
  /** The item's resolved options; a change re-renders the same instance. */
  options: WidgetOptions<K>;
};

/** A widget component of kind `K`. */
export type WidgetComponent<K extends BarWidgetKind = BarWidgetKind> = ComponentType<
  WidgetProps<K>
>;

/**
 * How to load one widget of kind `K`. `load` returns a dynamic import, so each
 * widget is its own chunk and a widget absent from the layout is never
 * downloaded.
 */
export type WidgetDefinition<K extends BarWidgetKind = BarWidgetKind> = {
  load: () => Promise<{ default: WidgetComponent<K> }>;
};

/**
 * Maps every widget kind to a definition typed for that kind, so a widget only
 * receives its own options. Exhaustive: a kind added in Rust (the generated
 * `BarWidgetKind`) fails type checking until it is registered.
 */
export type WidgetRegistry = { [K in BarWidgetKind]: WidgetDefinition<K> };

/**
 * A registry that may leave kinds out (tests, the end-to-end harness). The
 * layout engine accepts it; an unregistered kind renders nothing.
 */
export type PartialWidgetRegistry = Partial<WidgetRegistry>;
