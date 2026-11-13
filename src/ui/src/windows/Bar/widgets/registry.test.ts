import { describe, expect, expectTypeOf, it } from 'vitest';

import type { BarWidgetKind } from '@/ipc';

import { defaultWidgetOptions } from '../../../../tests/fixtures';

import { productionRegistry } from './registry';
import type { PartialWidgetRegistry, WidgetDefinition, WidgetRegistry } from './types';

describe('production registry', () => {
  it('registers every kind', () => {
    const kinds = Object.keys(defaultWidgetOptions).sort();

    expect(Object.keys(productionRegistry).sort()).toEqual(kinds);
  });

  it('is exhaustive at the type level', () => {
    expectTypeOf(productionRegistry).toEqualTypeOf<WidgetRegistry>();
    expectTypeOf<keyof WidgetRegistry>().toEqualTypeOf<BarWidgetKind>();
    // Every key is required: a missing kind is a type error.
    expectTypeOf<Required<WidgetRegistry>>().toEqualTypeOf<WidgetRegistry>();

    const { clock, ...withoutClock } = productionRegistry;

    expect(clock).toBeDefined();
    // @ts-expect-error -- `clock` is missing, so this is not a `WidgetRegistry`.
    const incomplete: WidgetRegistry = withoutClock;
    // A partial registry (tests, the harness) may leave kinds out.
    const partial: PartialWidgetRegistry = withoutClock;

    expect(incomplete).toBe(withoutClock);
    expect(partial).toBe(withoutClock);
  });

  it('types each definition for its own kind', () => {
    expectTypeOf(productionRegistry.clock).toEqualTypeOf<WidgetDefinition<'clock'>>();
  });
});
