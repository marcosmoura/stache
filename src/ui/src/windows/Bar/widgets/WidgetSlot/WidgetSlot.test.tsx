import { afterEach, describe, expect, it, vi } from 'vitest';

import type { BarWidgetKind } from '@/ipc';

import { fixtureRegistry } from '../../../../../tests/fixtureRegistry';
import { layoutItem } from '../../../../../tests/fixtures';
import { render } from '../../../../../tests/render';
import { CounterWidget } from '../../../../../tests/widgets';
import { WidgetRegistryProvider } from '../../providers';
import { productionRegistry } from '../registry';
import { lazyWidget, resolveWidget } from '../resolve';
import type { WidgetOptions, PartialWidgetRegistry, WidgetZone } from '../types';

import { WidgetSlot } from './WidgetSlot';

type Slot = Parameters<typeof WidgetSlot>[0] & { mount?: number };

function slot<K extends BarWidgetKind>(
  id: string,
  widget: K,
  zone: WidgetZone,
  options: Partial<WidgetOptions<K>> = {},
  mount = 0,
): Slot {
  return { ...layoutItem(id, widget, options), zone, mount };
}

function slotTree(registry: PartialWidgetRegistry, slots: Slot[]) {
  return (
    <WidgetRegistryProvider registry={registry}>
      {slots.map(({ mount = 0, ...slot }) => (
        <div key={`${slot.id}-${mount}`} data-testid={`slot-${slot.id}`}>
          <WidgetSlot {...slot} />
        </div>
      ))}
    </WidgetRegistryProvider>
  );
}

function renderSlot(registry: PartialWidgetRegistry, slots: Slot[]) {
  return render(slotTree(registry, slots));
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('WidgetSlot', () => {
  it('renders the lazy widget with its id and zone', async () => {
    const screen = await renderSlot(fixtureRegistry, [slot('clock', 'clock', 'end')]);
    const widget = screen.getByTestId('widget-clock');

    await expect.element(widget).toHaveTextContent('clock:0');
    await expect.element(widget).toHaveAttribute('data-zone', 'end');
  });

  it('passes the item options and re-renders the same instance on an options change', async () => {
    const screen = await renderSlot(fixtureRegistry, [
      slot('clock', 'clock', 'end'),
      slot('cpu', 'cpu', 'end', { temperature_alert: 90 }),
    ]);
    const clock = screen.getByTestId('widget-clock');

    await expect
      .element(clock)
      .toHaveAttribute(
        'data-options',
        JSON.stringify({ show_date: true, show_seconds: true, hour_cycle: 'h23' }),
      );
    await expect
      .element(screen.getByTestId('widget-cpu'))
      .toHaveAttribute('data-options', JSON.stringify({ temperature_alert: 90 }));
    await clock.click();
    await expect.element(clock).toHaveTextContent('clock:1');
    const before = clock.element();

    await screen.rerender(
      slotTree(fixtureRegistry, [
        slot('clock', 'clock', 'end', { show_seconds: false, hour_cycle: 'h12' }),
        slot('cpu', 'cpu', 'end', { temperature_alert: 70 }),
      ]),
    );

    await expect
      .element(clock)
      .toHaveAttribute(
        'data-options',
        JSON.stringify({ show_date: true, show_seconds: false, hour_cycle: 'h12' }),
      );
    await expect
      .element(screen.getByTestId('widget-cpu'))
      .toHaveAttribute('data-options', JSON.stringify({ temperature_alert: 70 }));
    await expect.element(clock).toHaveTextContent('clock:1');
    expect(clock.element()).toBe(before);
  });

  it('isolates a widget that throws and logs the failure once', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const screen = await renderSlot(fixtureRegistry, [
      slot('wifi', 'wifi', 'end'),
      slot('clock', 'clock', 'end'),
    ]);

    await expect.element(screen.getByTestId('widget-clock')).toBeVisible();
    await vi.waitFor(() => expect(ownLogs(error, 'wifi')).toBe(1));
    expect(screen.getByTestId('slot-wifi').element().childElementCount).toBe(0);
  });

  it('isolates a widget whose load rejects and logs the failure once', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const screen = await renderSlot(fixtureRegistry, [
      slot('battery', 'battery', 'end'),
      slot('cpu', 'cpu', 'end'),
    ]);

    await expect.element(screen.getByTestId('widget-cpu')).toBeVisible();
    await vi.waitFor(() => expect(ownLogs(error, 'battery')).toBe(1));
    expect(screen.getByTestId('slot-battery').element().childElementCount).toBe(0);
  });

  it('renders nothing for an unregistered kind and warns once per kind', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    // The fixture registry leaves keep_awake unregistered.
    const screen = await renderSlot(fixtureRegistry, [
      slot('a', 'keep_awake', 'start'),
      slot('b', 'keep_awake', 'end'),
    ]);

    expect(screen.getByTestId('slot-a').element().childElementCount).toBe(0);
    expect(screen.getByTestId('slot-b').element().childElementCount).toBe(0);
    expect(
      warn.mock.calls.filter(([message]) => String(message).includes('"keep_awake"')).length,
    ).toBe(1);
  });

  it('the production registry registers all eight kinds', () => {
    expect(Object.keys(productionRegistry).sort()).toEqual([
      'battery',
      'clock',
      'cpu',
      'keep_awake',
      'media',
      'spaces',
      'weather',
      'wifi',
    ]);
  });

  it('creates one lazy component per definition', () => {
    const definition = resolveWidget(fixtureRegistry, 'clock')!;

    expect(lazyWidget(definition)).toBe(lazyWidget(definition));
    expect(lazyWidget(definition)).not.toBe(lazyWidget(resolveWidget(fixtureRegistry, 'cpu')!));
  });

  it('recovers a failed slot when its id is reconfigured to another kind', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const screen = await renderSlot(fixtureRegistry, [slot('x', 'wifi', 'end')]);

    await vi.waitFor(() => expect(ownLogs(error, 'x')).toBe(1));
    expect(screen.getByTestId('slot-x').element().childElementCount).toBe(0);

    await screen.rerender(slotTree(fixtureRegistry, [slot('x', 'clock', 'end')]));

    await expect.element(screen.getByTestId('widget-x')).toHaveTextContent('x:0');
    expect(ownLogs(error, 'x')).toBe(1);
  });

  it('does not carry the old kind state into the new kind', async () => {
    const screen = await renderSlot(fixtureRegistry, [slot('x', 'clock', 'end')]);

    await screen.getByTestId('widget-x').click();
    await expect.element(screen.getByTestId('widget-x')).toHaveTextContent('x:1');

    await screen.rerender(slotTree(fixtureRegistry, [slot('x', 'cpu', 'end')]));

    await expect.element(screen.getByTestId('widget-x')).toHaveTextContent('x:0');
  });

  it('retries a rejected load on a later mount', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    let attempts = 0;
    const registry: PartialWidgetRegistry = {
      clock: {
        load: () => {
          attempts += 1;

          return attempts === 1
            ? Promise.reject(new Error('transient chunk failure'))
            : Promise.resolve({ default: CounterWidget });
        },
      },
    };
    const screen = await renderSlot(registry, [slot('c', 'clock', 'end')]);

    await vi.waitFor(() => expect(ownLogs(error, 'c')).toBe(1));
    expect(screen.getByTestId('slot-c').element().childElementCount).toBe(0);
    expect(attempts).toBe(1);

    await screen.rerender(slotTree(registry, [slot('c', 'clock', 'end', {}, 1)]));

    await expect.element(screen.getByTestId('widget-c')).toHaveTextContent('c:0');
    expect(attempts).toBe(2);
    expect(ownLogs(error, 'c')).toBe(1);
  });
});

function ownLogs(spy: { mock: { calls: unknown[][] } }, id: string) {
  return spy.mock.calls.filter(([message]) => String(message).includes(`widget "${id}"`)).length;
}
