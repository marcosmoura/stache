import { Profiler } from 'react';

import { MotionGlobalConfig } from 'motion/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import type {
  BarLayoutView,
  BarSnapshotOutcome_Serialize,
  BarView_Serialize,
  BarWidgetKind,
  MenuBarState,
} from '@/ipc';
import { ticker } from '@/stores';

import { fixtureRegistry } from '../../../tests/fixtureRegistry';
import {
  barAvailable,
  barChanged,
  barUnauthorized,
  barUnavailable,
  defaultLayoutFixture,
  layoutItem,
} from '../../../tests/fixtures';
import { mockHost, type MockHost } from '../../../tests/ipc';
import { installReducedMotionQuery, setReducedMotion } from '../../../tests/reducedMotion';
import { render } from '../../../tests/render';

import { Bar } from './Bar';
import { WidgetRegistryProvider } from './providers';
import {
  productionRegistry,
  type WidgetComponent,
  type WidgetProps,
  type PartialWidgetRegistry,
} from './widgets';

type Zone = 'start' | 'center' | 'end';

const item = (id: string, widget: BarWidgetKind = 'clock') => layoutItem(id, widget);

async function renderBar(host: MockHost, registry?: PartialWidgetRegistry) {
  const bar = <Bar surface="bar" />;
  const screen = await render(
    registry ? <WidgetRegistryProvider registry={registry}>{bar}</WidgetRegistryProvider> : bar,
  );

  await vi.waitFor(() => expect(host.listenerCount('bar-changed')).toBe(1));

  return screen;
}

function zone(name: Zone) {
  const element = document.querySelector<HTMLElement>(`[data-zone="${name}"]`);

  if (!element) {
    throw new Error(`no ${name} zone`);
  }

  return element;
}

const itemsOf = (name: Zone) =>
  [...zone(name).querySelectorAll<HTMLElement>('[data-item]')].map((slot) => slot.dataset.item);

const layout = (start: string[][], center: string[][], end: string[][]): BarLayoutView => {
  const map = (entries: string[][]) =>
    entries.map(([id, widget]) => item(id!, (widget ?? 'clock') as never));

  return { start: map(start), center: map(center), end: map(end) };
};

installReducedMotionQuery();

afterEach(() => {
  vi.restoreAllMocks();
  setReducedMotion(false);
});

describe('Bar', () => {
  it('renders each zone in configured order', async () => {
    const host = mockHost().answer(
      'bar_snapshot',
      barAvailable('1', {
        layout: layout(
          [['spaces', 'spaces'], ['clock']],
          [['media', 'media']],
          [
            ['cpu', 'cpu'],
            ['weather', 'weather'],
          ],
        ),
      }),
    );
    const screen = await renderBar(host, fixtureRegistry);

    await expect.element(screen.getByTestId('widget-clock')).toBeVisible();
    expect(itemsOf('start')).toEqual(['spaces', 'clock']);
    expect(itemsOf('center')).toEqual(['media']);
    expect(itemsOf('end')).toEqual(['cpu', 'weather']);
    await expect.element(screen.getByTestId('widget-cpu')).toHaveAttribute('data-zone', 'end');
    expect(zone('start').getBoundingClientRect().left).toBeLessThan(
      zone('end').getBoundingClientRect().left,
    );
  });

  it('applies the accepted height', async () => {
    const host = mockHost().answer('bar_snapshot', barAvailable('1', { height: 37, padding: 5 }));

    await renderBar(host, fixtureRegistry);
    await vi.waitFor(() =>
      expect(zone('start').parentElement!.getBoundingClientRect().height).toBe(37),
    );
  });

  it('keeps the center zone centered when start and end differ in width', async () => {
    // Wide enough for both side zones to fit in their equal shares.
    await page.viewport(1600, 200);

    const host = mockHost().answer(
      'bar_snapshot',
      barAvailable('1', {
        layout: layout(
          [
            ['wide_a', 'spaces'],
            ['wide_b', 'weather'],
          ],
          [['media', 'media']],
          [['cpu', 'cpu']],
        ),
      }),
    );
    const screen = await renderBar(host, fixtureRegistry);

    await expect.element(screen.getByTestId('widget-media')).toBeVisible();
    await expect.element(screen.getByTestId('widget-cpu')).toBeVisible();

    const root = zone('center').parentElement!.getBoundingClientRect();
    const media = screen.getByTestId('widget-media').element().getBoundingClientRect();
    const start = zone('start').getBoundingClientRect();
    const end = zone('end').getBoundingClientRect();
    const startWidth = screen.getByTestId('widget-wide_a').element().getBoundingClientRect().width;

    expect(startWidth).toBeGreaterThan(100);
    expect(Math.abs(media.left + media.width / 2 - (root.left + root.width / 2))).toBeLessThan(1);
    expect(start.left).toBeCloseTo(root.left, 0);
    expect(end.right).toBeCloseTo(root.right, 0);

    // Too narrow for equal shares: the side zones keep their content and the
    // center zone gives way, so nothing is squeezed or overlaps.
    await page.viewport(414, 200);
    await vi.waitFor(() => {
      const narrowStart = zone('start').getBoundingClientRect();
      const narrowCenter = zone('center').getBoundingClientRect();
      const wideA = screen.getByTestId('widget-wide_a').element().getBoundingClientRect();

      expect(wideA.width).toBe(startWidth);
      expect(narrowCenter.left).toBeGreaterThanOrEqual(narrowStart.right);
    });
    await page.viewport(414, 896);
  });

  it('keeps an item state across reorder and zone moves, unmounts removed and mounts added', async () => {
    const host = mockHost().answer(
      'bar_snapshot',
      barAvailable('1', { layout: layout([['clock']], [['media', 'media']], [['cpu', 'cpu']]) }),
    );
    const screen = await renderBar(host, fixtureRegistry);
    const clock = screen.getByTestId('widget-clock');

    await clock.click();
    await clock.click();
    await expect.element(clock).toHaveTextContent('clock:2');
    await screen.getByTestId('widget-cpu').click();
    await expect.element(screen.getByTestId('widget-cpu')).toHaveTextContent('cpu:1');

    // Reorder inside a zone.
    host.emit(
      'bar-changed',
      barChanged(
        '2',
        barAvailable('2', {
          layout: layout([], [['media', 'media']], [['clock'], ['cpu', 'cpu']]),
        }),
      ),
    );
    await vi.waitFor(() => expect(itemsOf('end')).toEqual(['clock', 'cpu']));
    await expect.element(clock).toHaveTextContent('clock:2');

    // Move across zones, remove media, add a second clock.
    host.emit(
      'bar-changed',
      barChanged(
        '3',
        barAvailable('3', { layout: layout([['cpu', 'cpu']], [['clock']], [['clock_2']]) }),
      ),
    );
    await vi.waitFor(() => expect(itemsOf('start')).toEqual(['cpu']));
    await vi.waitFor(() => expect(itemsOf('center')).toEqual(['clock']));
    await expect.element(screen.getByTestId('widget-clock')).toHaveTextContent('clock:2');
    await expect.element(screen.getByTestId('widget-clock')).toHaveAttribute('data-zone', 'center');
    await expect.element(screen.getByTestId('widget-cpu')).toHaveTextContent('cpu:1');
    await expect.element(screen.getByTestId('widget-cpu')).toHaveAttribute('data-zone', 'start');
    await expect.element(screen.getByTestId('widget-clock_2')).toHaveTextContent('clock_2:0');
    await expect.element(screen.getByTestId('widget-media')).not.toBeInTheDocument();
    expect(
      zone('center')
        .querySelector('[data-item="clock"]')!
        .contains(screen.getByTestId('widget-clock').element()),
    ).toBe(true);
  });

  it('delivers an options change to the same instance, keeping its state', async () => {
    const options = (id: string) => JSON.parse(widget(id).dataset.options ?? 'null') as unknown;
    const widget = (id: string) =>
      document.querySelector<HTMLElement>(`[data-testid="widget-${id}"]`)!;
    const host = mockHost().answer(
      'bar_snapshot',
      barAvailable('1', {
        layout: {
          start: [],
          center: [],
          end: [layoutItem('cpu', 'cpu'), layoutItem('clock', 'clock')],
        },
      }),
    );
    const screen = await renderBar(host, fixtureRegistry);
    const clock = screen.getByTestId('widget-clock');

    await clock.click();
    await expect.element(clock).toHaveTextContent('clock:1');
    expect(options('clock')).toEqual({ show_date: true, show_seconds: true, hour_cycle: 'h23' });
    expect(options('cpu')).toEqual({ temperature_alert: 85 });
    const before = widget('clock');

    host.emit(
      'bar-changed',
      barChanged(
        '2',
        barAvailable('2', {
          layout: {
            start: [],
            center: [],
            end: [
              layoutItem('cpu', 'cpu', { temperature_alert: 95 }),
              layoutItem('clock', 'clock', { show_date: false, hour_cycle: 'h12' }),
            ],
          },
        }),
      ),
    );
    await vi.waitFor(() =>
      expect(options('clock')).toEqual({ show_date: false, show_seconds: true, hour_cycle: 'h12' }),
    );
    expect(options('cpu')).toEqual({ temperature_alert: 95 });
    await expect.element(clock).toHaveTextContent('clock:1');
    expect(widget('clock')).toBe(before);
    expect(document.querySelector('[data-exiting]')).toBeNull();
  });

  it('renders both instances of a repeated widget with distinct ids', async () => {
    const host = mockHost().answer(
      'bar_snapshot',
      barAvailable('1', { layout: layout([['clock'], ['clock_2']], [], []) }),
    );
    const screen = await renderBar(host, fixtureRegistry);

    await expect.element(screen.getByTestId('widget-clock')).toHaveTextContent('clock:0');
    await expect.element(screen.getByTestId('widget-clock_2')).toHaveTextContent('clock_2:0');
  });

  it('isolates a throwing widget and a failed load, leaving empty slots', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const host = mockHost().answer(
      'bar_snapshot',
      barAvailable('1', {
        layout: layout([['wifi', 'wifi']], [['battery', 'battery']], [['clock']]),
      }),
    );
    const screen = await renderBar(host, fixtureRegistry);

    await expect.element(screen.getByTestId('widget-clock')).toHaveTextContent('clock:0');
    await vi.waitFor(() => {
      expect(logsOf(error, 'wifi')).toBe(1);
      expect(logsOf(error, 'battery')).toBe(1);
    });
    expect(itemsOf('start')).toEqual(['wifi']);
    expect(itemsOf('center')).toEqual(['battery']);
    expect(zone('start').textContent).toBe('');
    expect(zone('center').textContent).toBe('');
    expect(document.querySelector('[role="alert"]')).toBeNull();
  });

  it('leaves no element for an unregistered kind', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const host = mockHost().answer(
      'bar_snapshot',
      barAvailable('1', { layout: layout([['keep_awake', 'keep_awake'], ['clock']], [], []) }),
    );
    const screen = await renderBar(host, fixtureRegistry);

    await expect.element(screen.getByTestId('widget-clock')).toBeVisible();
    expect(itemsOf('start')).toEqual(['clock']);
  });

  it('renders the registered widgets of the default layout with the production registry', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const host = mockHost()
      .answer('bar_snapshot', barAvailable('1', { layout: defaultLayoutFixture }))
      .answer('bar_weather', {
        kind: 'ready',
        weather: {
          configRevision: '1',
          provider: 'visualCrossing',
          apiKey: null,
          location: { source: 'none' },
          refreshIntervalMinutes: 20,
        },
      });

    await renderBar(host);
    await vi.waitFor(() => expect(host.callsOf('bar_snapshot')).toHaveLength(1));
    // Every kind is registered; Spaces draws nothing without Tiling.
    expect(itemsOf('start')).toEqual(['spaces']);
    expect(zone('start').childElementCount).toBe(1);
    expect(zone('start').textContent).toBe('');
    expect(itemsOf('center')).toEqual(['media']);
    expect(itemsOf('end')).toEqual(['weather', 'cpu', 'battery', 'keep_awake', 'wifi', 'clock']);
    // No readings and no weather configured: only the UI-local Clock draws.
    expect(zone('center').querySelectorAll('button')).toHaveLength(0);
    await vi.waitFor(() => expect(zone('end').querySelectorAll('button')).toHaveLength(1));
  });

  for (const [label, outcome] of [
    ['unavailable', barUnavailable('1')],
    ['unauthorized', barUnauthorized],
  ] as const) {
    it(`renders empty zones and no error UI while ${label}, then items once available`, async () => {
      const host = mockHost().answer('bar_snapshot', outcome);
      const screen = await renderBar(host, fixtureRegistry);

      await vi.waitFor(() => expect(host.callsOf('bar_snapshot')).toHaveLength(1));
      expectEmptyZones();
      host.emit(
        'bar-changed',
        barChanged('5', barAvailable('5', { layout: layout([['clock']], [], []) })),
      );
      await expect.element(screen.getByTestId('widget-clock')).toBeVisible();
    });
  }

  it('renders empty zones before the first snapshot', async () => {
    const host = mockHost().answer('bar_snapshot', () => new Promise(() => undefined));

    await renderBar(host, fixtureRegistry);
    expectEmptyZones();
  });
});

describe('Bar layout animations', () => {
  let sequence = 10;

  const emitLayout = (host: MockHost, next: BarLayoutView) => {
    sequence += 1;
    const seq = String(sequence);

    host.emit('bar-changed', barChanged(seq, barAvailable(seq, { layout: next })));
  };

  async function animatedBar(initial: BarLayoutView, reduce = false) {
    MotionGlobalConfig.skipAnimations = false;
    setReducedMotion(reduce);
    const host = mockHost().answer('bar_snapshot', barAvailable('1', { layout: initial }));
    const screen = await renderBar(host, fixtureRegistry);

    await expect.element(screen.getByTestId('widget-clock')).toBeVisible();

    return { host, screen };
  }

  const widgets = (id: string) => document.querySelectorAll(`[data-testid="widget-${id}"]`);

  it('keeps a removed widget in its exiting box until the exit completes', async () => {
    const { host } = await animatedBar(layout([['clock'], ['cpu', 'cpu']], [], []));

    // Every frame the exiting box is on screen, it still shows the widget.
    const frames: boolean[] = [];
    let sampling = true;
    const sample = () => {
      const exiting = document.querySelector('[data-exiting="cpu"]');

      if (exiting) {
        frames.push(exiting.querySelector('[data-testid="widget-cpu"]') !== null);
      }

      if (sampling) {
        requestAnimationFrame(sample);
      }
    };

    requestAnimationFrame(sample);
    emitLayout(host, layout([['clock']], [], []));
    await vi.waitFor(() => expect(frames.length).toBeGreaterThan(2));
    await vi.waitFor(() => {
      expect(widgets('cpu')).toHaveLength(0);
      expect(document.querySelector('[data-exiting]')).toBeNull();
    });
    sampling = false;
    expect(frames.every(Boolean)).toBe(true);
    expect(itemsOf('start')).toEqual(['clock']);
  });

  it('moves a surviving id across zones with its state and never at opacity 0', async () => {
    const { host, screen } = await animatedBar(layout([['clock']], [], [['cpu', 'cpu']]));
    const clock = screen.getByTestId('widget-clock');

    await clock.click();
    await expect.element(clock).toHaveTextContent('clock:1');

    const samples: string[] = [];
    let sampling = true;
    const sample = () => {
      const slot = document.querySelector('[data-zone="end"] [data-item="clock"]');

      if (slot) {
        samples.push(getComputedStyle(slot).opacity);
      }

      if (sampling) {
        requestAnimationFrame(sample);
      }
    };

    emitLayout(host, layout([], [], [['cpu', 'cpu'], ['clock']]));
    requestAnimationFrame(sample);
    await vi.waitFor(() => expect(itemsOf('end')).toEqual(['cpu', 'clock']));
    await new Promise((resolve) => setTimeout(resolve, 400));
    sampling = false;

    expect(samples.length).toBeGreaterThan(0);
    expect(samples.every((opacity) => opacity === '1')).toBe(true);
    await expect.element(clock).toHaveTextContent('clock:1');
    await expect.element(clock).toHaveAttribute('data-zone', 'end');
    expect(widgets('clock')).toHaveLength(1);
  });

  it('keeps a moved-then-removed widget until the exit of the zone it left completes', async () => {
    const { host } = await animatedBar(layout([['clock'], ['cpu', 'cpu']], [], []));

    // While the end box fades out, it still shows the widget on every frame,
    // even after the older start box (left by the move) finishes its exit.
    const frames: boolean[] = [];
    let sampling = true;
    const sample = () => {
      // Queried directly: a frame can still fire after the test's cleanup.
      const exiting = document.querySelector('[data-zone="end"] [data-exiting="cpu"]');

      if (exiting) {
        frames.push(exiting.querySelector('[data-testid="widget-cpu"]') !== null);
      }

      if (sampling) {
        requestAnimationFrame(sample);
      }
    };

    requestAnimationFrame(sample);
    emitLayout(host, layout([['clock']], [], [['cpu', 'cpu']]));
    await vi.waitFor(() => expect(itemsOf('end')).toEqual(['cpu']), { interval: 5 });
    expect(zone('start').querySelector('[data-exiting="cpu"]')).not.toBeNull();
    await new Promise((resolve) => setTimeout(resolve, 60));
    emitLayout(host, layout([['clock']], [], []));
    await vi.waitFor(() => {
      expect(widgets('cpu')).toHaveLength(0);
      expect(document.querySelector('[data-exiting]')).toBeNull();
    });
    sampling = false;
    expect(frames.length).toBeGreaterThan(2);
    expect(frames.every(Boolean)).toBe(true);
  });

  for (const [label, readded] of [
    ['the same zone', layout([['clock'], ['cpu', 'cpu']], [], [])],
    ['another zone', layout([['clock']], [], [['cpu', 'cpu']])],
  ] as const) {
    it(`ends with one instance when an id is removed and quickly re-added to ${label}`, async () => {
      const { host, screen } = await animatedBar(layout([['clock'], ['cpu', 'cpu']], [], []));

      await screen.getByTestId('widget-cpu').click();
      emitLayout(host, layout([['clock']], [], []));
      await vi.waitFor(
        () => expect(document.querySelector('[data-exiting="cpu"]')).not.toBeNull(),
        {
          interval: 5,
        },
      );
      emitLayout(host, readded);

      const target = readded.end.length > 0 ? 'end' : 'start';

      await vi.waitFor(() => {
        expect(document.querySelector('[data-exiting]')).toBeNull();
        expect(widgets('cpu')).toHaveLength(1);
        expect(
          zone(target).querySelector('[data-item="cpu"] [data-testid="widget-cpu"]'),
        ).not.toBeNull();
      });
      await expect.element(screen.getByTestId('widget-cpu')).toHaveTextContent('cpu:1');
      await expect.element(screen.getByTestId('widget-cpu')).toHaveAttribute('data-zone', target);
    });
  }

  it('removes at once under reduced motion', async () => {
    const { host } = await animatedBar(layout([['clock'], ['cpu', 'cpu']], [], []), true);

    emitLayout(host, layout([['clock']], [], []));
    await vi.waitFor(
      () => {
        expect(widgets('cpu')).toHaveLength(0);
        expect(document.querySelector('[data-exiting]')).toBeNull();
      },
      { timeout: 180, interval: 5 },
    );
  });
});

const barRoot = () => document.querySelector<HTMLElement>('[data-surface="bar"]')!;
const barWindow = () => document.querySelector<HTMLElement>('[data-bar-window]')!;

/** The Bar's vertical offset (px) and opacity, as rendered. */
const placement = () => {
  const style = getComputedStyle(barRoot());

  return { y: new DOMMatrix(style.transform).m42, opacity: Number(style.opacity) };
};

const withMenu = (sequence: string, state: MenuBarState) =>
  barAvailable(sequence, { menu: { state, reason: null } });

describe('Bar slide-away', () => {
  const outcomes: Array<[string, BarSnapshotOutcome_Serialize, boolean]> = [
    ['available, menu visible', withMenu('1', 'visible'), true],
    ['available, menu hidden', withMenu('1', 'hidden'), false],
    ['available, menu unknown', withMenu('1', 'unknown'), false],
    ['unavailable', barUnavailable('1'), false],
    ['unauthorized', barUnauthorized, false],
  ];

  for (const [name, outcome, away] of outcomes) {
    it(`${away ? 'slides away' : 'stays shown'} for ${name}`, async () => {
      const host = mockHost().answer('bar_snapshot', outcome);

      await renderBar(host);
      await vi.waitFor(() => expect(host.callsOf('bar_snapshot')).toHaveLength(1));
      await expect
        .poll(placement, { timeout: 1000 })
        .toEqual(away ? { y: 32, opacity: 0 } : { y: 0, opacity: 1 });
      expect(barRoot().hasAttribute('data-away')).toBe(away);
    });
  }

  it('stays shown before the first snapshot', async () => {
    const host = mockHost().answer('bar_snapshot', () => new Promise(() => {}));

    await renderBar(host);
    expect(placement()).toEqual({ y: 0, opacity: 1 });
  });

  it('leaves and returns with the menu state, by its own height, over the motion duration', async () => {
    MotionGlobalConfig.skipAnimations = false;
    const host = mockHost().answer('bar_snapshot', barAvailable('1', { height: 40 }));

    await renderBar(host);
    await vi.waitFor(() => expect(host.callsOf('bar_snapshot')).toHaveLength(1));
    expect(placement()).toEqual({ y: 0, opacity: 1 });

    const started = performance.now();

    host.emit(
      'bar-changed',
      barChanged('2', barAvailable('2', { height: 40, menu: { state: 'visible', reason: null } })),
    );
    await vi.waitFor(() => expect(barRoot().hasAttribute('data-away')).toBe(true));
    // Mid-slide: not there yet.
    await vi.waitFor(() => expect(placement().y).toBeGreaterThan(0), { interval: 5 });
    expect(placement().y).toBeLessThan(40);
    await expect.poll(placement).toEqual({ y: 40, opacity: 0 });
    expect(performance.now() - started).toBeGreaterThanOrEqual(100);

    for (const [sequence, outcome] of [
      ['3', withMenu('3', 'hidden')],
      ['4', withMenu('4', 'visible')],
      ['5', barUnavailable('5')],
    ] as const) {
      host.emit('bar-changed', barChanged(sequence, outcome));
    }

    await expect.poll(placement).toEqual({ y: 0, opacity: 1 });
  });

  it('moves instantly under reduced motion', async () => {
    MotionGlobalConfig.skipAnimations = false;
    setReducedMotion(true);
    const host = mockHost().answer('bar_snapshot', withMenu('1', 'hidden'));

    await renderBar(host);
    await vi.waitFor(() => expect(host.callsOf('bar_snapshot')).toHaveLength(1));

    host.emit('bar-changed', barChanged('2', withMenu('2', 'visible')));
    await vi.waitFor(() => expect(barRoot().hasAttribute('data-away')).toBe(true));
    await vi.waitFor(() => expect(placement()).toEqual({ y: 32, opacity: 0 }), {
      timeout: 60,
      interval: 5,
    });

    host.emit('bar-changed', barChanged('3', withMenu('3', 'hidden')));
    await vi.waitFor(() => expect(barRoot().hasAttribute('data-away')).toBe(false));
    await vi.waitFor(() => expect(placement()).toEqual({ y: 0, opacity: 1 }), {
      timeout: 60,
      interval: 5,
    });
  });
});

describe('Bar window rules', () => {
  it('disables the context menu', async () => {
    const host = mockHost().answer('bar_snapshot', barAvailable('1'));

    await renderBar(host);

    const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });

    zone('end').dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
  });

  it('forbids text selection and sets the Bar text style', async () => {
    const host = mockHost().answer('bar_snapshot', barAvailable('1'));

    await renderBar(host);

    const style = getComputedStyle(barWindow());

    expect(style.webkitUserSelect).toBe('none');
    expect(style.fontSize).toBe('13px');
    expect(style.fontWeight).toBe('500');
    expect(style.fontVariantNumeric).toBe('tabular-nums');
    expect(style.fontFamily).toContain('Maple Mono NF');
    expect(style.overflow).toBe('hidden');
    expect(barWindow().getBoundingClientRect().height).toBe(window.innerHeight);
  });

  it('separates items in a zone by the item gap', async () => {
    const host = mockHost().answer(
      'bar_snapshot',
      barAvailable('1', { layout: layout([], [], [['cpu', 'cpu'], ['clock']]) }),
    );
    const screen = await renderBar(host, fixtureRegistry);

    await expect.element(screen.getByTestId('widget-clock')).toBeVisible();
    expect(getComputedStyle(zone('end')).columnGap).toBe('4px');
  });
});

/*
 * Each widget selects only its own reading, so a snapshot change in one
 * reading re-renders only that widget (MIG-03-06). The Clock re-renders only
 * on its own ticks, never on a snapshot change. Measured through the real Bar
 * and the production widgets, each wrapped in a `Profiler` by a test registry.
 */
describe('Bar widget render isolation', () => {
  type Counted = 'cpu' | 'battery' | 'keep_awake' | 'wifi' | 'clock';

  const READINGS: Partial<BarView_Serialize> = {
    layout: defaultLayoutFixture,
    cpu: { usage: 10, temperature: 40 },
    battery: { percentage: 80, state: 'Discharging' },
    keepAwake: { active: true, paused: false, enabled: true },
    wifi: { status: 'Connected', networkName: 'Home', signalStrength: -50 },
  };

  const CHANGES: Array<[Exclude<Counted, 'clock'>, Partial<BarView_Serialize>]> = [
    ['cpu', { cpu: { usage: 11, temperature: 40 } }],
    ['battery', { battery: { percentage: 79, state: 'Discharging' } }],
    ['keep_awake', { keepAwake: { active: false, paused: false, enabled: true } }],
    ['wifi', { wifi: { status: 'Off', networkName: null, signalStrength: null } }],
  ];

  let stopTicks: (() => void) | null = null;

  afterEach(() => {
    stopTicks?.();
    stopTicks = null;
  });

  const renders: Record<Counted, number> = { cpu: 0, battery: 0, keep_awake: 0, wifi: 0, clock: 0 };

  const countRender = (id: string) => {
    renders[id as Counted] += 1;
  };

  /** The production widget of `kind`, wrapped in a `Profiler` counting its renders. */
  const profiled = (kind: Counted) => ({
    load: async () => {
      const { default: Widget } = await productionRegistry[kind]!.load();

      function ProfiledWidget(props: WidgetProps) {
        return (
          <Profiler id={kind} onRender={countRender}>
            <Widget {...(props as WidgetProps<never>)} />
          </Profiler>
        );
      }

      return { default: ProfiledWidget as WidgetComponent };
    },
  });

  /** The production registry with each widget wrapped in a counting `Profiler`. */
  const profiledRegistry = {
    cpu: profiled('cpu'),
    battery: profiled('battery'),
    keep_awake: profiled('keep_awake'),
    wifi: profiled('wifi'),
    clock: profiled('clock'),
  } as PartialWidgetRegistry;

  it('a change in one reading re-renders only its widget', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    let ticks = 0;

    stopTicks = ticker.subscribe(() => {
      ticks += 1;
    });

    const host = mockHost().answer('bar_snapshot', barAvailable('1', READINGS));

    await renderBar(host, profiledRegistry);
    await vi.waitFor(() => expect(zone('end').querySelectorAll('button')).toHaveLength(5));

    let view = { ...READINGS };
    let sequence = 1;

    for (const [name, change] of CHANGES) {
      // Let the ScrollingLabel measurement and any pending render settle.
      await new Promise((done) => setTimeout(done, 50));
      const before = { ...renders };
      const ticksBefore = ticks;

      sequence += 1;
      view = { ...view, ...change };
      host.emit('bar-changed', barChanged(String(sequence), barAvailable(String(sequence), view)));
      await expect.poll(() => renders[name]).toBeGreaterThan(before[name]);
      await new Promise((done) => setTimeout(done, 50));

      for (const other of ['cpu', 'battery', 'keep_awake', 'wifi'] as const) {
        if (other !== name) {
          expect(renders[other], `${other} after a ${name} change`).toBe(before[other]);
        }
      }

      expect(renders.clock - before.clock, `clock after a ${name} change`).toBe(
        ticks - ticksBefore,
      );
    }
  });
});

function expectEmptyZones() {
  expect(document.querySelectorAll('[data-zone]')).toHaveLength(3);
  expect(document.querySelectorAll('[data-item]')).toHaveLength(0);
  expect(document.querySelector('[role="alert"]')).toBeNull();
}

function logsOf(spy: { mock: { calls: unknown[][] } }, id: string) {
  return spy.mock.calls.filter(([message]) => String(message).includes(`widget "${id}"`)).length;
}
