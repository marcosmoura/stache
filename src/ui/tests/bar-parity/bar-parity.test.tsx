import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { commands } from 'vitest/browser';

import * as surfaceMotion from '../../src/shell/components/Surface/Surface.motion';
import { fixtures, harnessSources, manifest, referenceFiles } from '../bar-reference/reference';
import { LAPTOP_MEDIA_QUERY } from '../bar-reference/render';
import { verifyReference } from '../bar-reference/verify';

import { compareCaptures, describeDiffs, type DiffCategory } from './capture';
import { checkFont, environmentRecord, type FontCheck } from './environment';
import {
  applyTimeline,
  nextFrame,
  renderNew,
  renderReference,
  setViewport,
  snapshot,
  untilStable,
  type FrameConfig,
  type NewRendered,
  type Rendered,
  type Snapshot,
  type Timeline,
} from './harness';
/**
 * Bar visual-parity gate (spec 2026-11-03-bar/04, BAR-04-02/03).
 *
 * For each scenario of the independent legacy reference, the reference render and the new Bar are
 * captured in identical iframes and compared exactly: geometry, every computed style, SVG
 * attributes and screenshots. No threshold, no masking, no stored baseline: both renders come from
 * the same run. The same file runs at device scale 1 (project `ui`) and 2 (`bar-parity-2x`).
 */
import { freezeClock, releaseClock, stepClock } from './motionClock';
import { buildScenarios, type ParityScenario } from './scenarios';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(), once: vi.fn(), emit: vi.fn() }));
// Lets one scenario put the real Bar into legacy's "Spaces suspended" state: `Spaces` renders the
// shipped `SpacesFallback` instead of its (never suspending) content.
vi.mock('../../src/shell/windows/Bar/Spaces', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/shell/windows/Bar/Spaces')>();
  const { SpacesFallback } = await import('../../src/shell/windows/Bar/Spaces/Spaces');
  const { spaces } = await import('./spacesFlag');

  return {
    ...actual,
    Spaces: () => (spaces.fallback ? <SpacesFallback /> : <actual.Spaces />),
  };
});

const SCALE = window.devicePixelRatio;

const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
const TIMEOUT = 60_000;

/** Pinned legacy motion (`_legacy/app/ui/design-system/motion.ts` at e0b4648): durations in ms. */
const LEGACY_MOTION = {
  easing: 'cubic-bezier(0.42, 0, 0.58, 1)',
  duration: 150,
  /** `motionRaw.durationSlow` = 0.275 s; the 400ms in `motion.durationSlow` is a CSS string never used by Surface. */
  surfaceFade: 275,
};

interface Dimension {
  id: string;
  display: number;
  padding: number;
  height: number;
  width: number;
  /** Every one of the 65 scenarios (otherwise the representative subset). */
  full: boolean;
}

function dimension(
  id: string,
  display: number,
  padding: number,
  height: number,
  full: boolean,
): Dimension {
  return { id, display, padding, height, width: display - 2 * padding, full };
}

const DIMENSIONS: Dimension[] = [
  dimension('laptop-1440-pad12-h28', 1440, 12, 28, true),
  dimension('external-2560-pad12-h28', 2560, 12, 28, true),
  dimension('laptop-1440-pad14-h28', 1440, 14, 28, false),
  dimension('external-2560-pad14-h28', 2560, 14, 28, false),
  dimension('laptop-1440-pad12-h32', 1440, 12, 32, false),
  dimension('external-2560-pad14-h32', 2560, 14, 32, false),
  // The legacy breakpoint is (width <= 2036px) on the CSS viewport, which is the display width
  // minus the panel padding on both sides (default 12).
  dimension('breakpoint-viewport-2035', 2035 + 24, 12, 28, true),
  dimension('breakpoint-viewport-2036', 2036 + 24, 12, 28, true),
  dimension('breakpoint-viewport-2037', 2037 + 24, 12, 28, true),
];

/** Scenarios that cover every component, branch family and breakpoint-sensitive element. */
const SUBSET = [
  'bar-default',
  'bar-fade-initial',
  'menu-bar-hidden',
  'global-error',
  'global-suspense',
  'spaces-fallback',
  'media-paused',
  'media-artwork-loaded',
  'media-long-title-scrolling',
  'battery-discharging-75',
  'battery-charging',
  'battery-label-loading',
  'cpu-hot',
  'wifi-off',
  'wifi-long-ssid-scrolling',
  'weather-loading',
  'weather-icon-snow',
  'keep-awake-active',
];
const SCROLLING = ['media-long-title', 'media-long-title-scrolling', 'wifi-long-ssid-scrolling'];

const artwork = /data:image\/png;base64,([A-Za-z0-9+/=]+)/u.exec(
  fixtures['media-artwork-loaded']?.wide ?? '',
)?.[1];

if (!artwork) {
  throw new Error('Reference media-artwork-loaded fixture has no artwork to reuse');
}

const scenarios = buildScenarios(artwork);
const byName = new Map(scenarios.map((scenario) => [scenario.name, scenario]));

function scenarioOf(name: string): ParityScenario {
  const found = byName.get(name);

  if (!found) {
    throw new Error(`No parity scenario ${name}`);
  }

  return found;
}

const counters = {
  cases: 0,
  elementsCompared: 0,
  screenshots: 0,
  groups: {} as Record<string, number>,
};
let font: FontCheck;
const open: Rendered[] = [];

function track<T extends Rendered>(rendered: T): T {
  open.push(rendered);

  return rendered;
}

function frameOf(dim: Dimension): FrameConfig {
  return { width: dim.width, height: dim.height };
}

beforeAll(async () => {
  await commands.parityEmulate('dark', 'no-preference');
  // Local fields, so the label reads the same in any time zone; only `Date` is faked.
  vi.useFakeTimers({ toFake: ['Date'] });
  vi.setSystemTime(new Date(2026, 10, 10, 14, 5, 9));
  font = await checkFont();

  if (!font.found) {
    throw new Error(font.message);
  }
});

afterEach(async () => {
  for (const rendered of open.splice(0)) {
    rendered.unmount();
  }

  await commands.parityPointer('reset', '', '', 0);
});

/** What the gate had to account for in the reference harness (reported to the user for review). */
const REFERENCE_NOTES = [
  'Reference page shell omits <html lang="en"> (legacy index.html:2 has it); mirrored by the harness.',
  'Reference markup is static, so JSX interpolations share one text node; Cpu spans are split like legacy Cpu.tsx:12-13 builds them (pixel-visible at wide widths).',
  'Reference scrolling-label numbers are fixed; the legacy measurement formula is re-applied on the reference layout (wifi scenario also leaves a stale scroll state on the Weather label).',
  'media-long-title is a pre-measurement transient in the reference; compared in its legacy settled (scrolling) state.',
  'Stylis -webkit-box-align/-webkit-box-pack duplicates are not generated by the reference flattener; ignored by name, the standard properties are compared.',
  'Linaria hashes the scroll-text keyframes name; the name is normalized, the keyframe rules are compared.',
  'The Surface fade is JS-driven (motion); sampled with a frozen clock (see motionClock.ts).',
  'Battery "Loading..." (percentage without state) is reachable only with a cast: the seam type requires state.',
];

afterAll(async () => {
  vi.useRealTimers();

  const environment = environmentRecord(font);

  await commands.parityEmulate('light', 'no-preference');

  const record = {
    schema: 1,
    note: 'Deterministic parity-run record. Generated by bar-parity.test.tsx; not the reference manifest.',
    referenceCommit: manifest.pinnedCommit,
    ...environment,
    referenceNotes: REFERENCE_NOTES,
    matrix: {
      scenarios: scenarios.length,
      dimensions: DIMENSIONS.map(({ id, display, padding, height, width, full }) => ({
        id,
        displayWidth: display,
        padding,
        panelViewportWidth: width,
        height,
        scenarios: full ? 'all' : SUBSET.length,
      })),
      cases: counters.cases,
      elementsCompared: counters.elementsCompared,
      screenshotsCompared: counters.screenshots,
      groups: counters.groups,
    },
  };

  await commands.parityWriteRecord(
    `run-record.${SCALE}x.json`,
    `${JSON.stringify(record, null, 2)}\n`,
  );
});

function count(group: string, snapshots: Snapshot[]) {
  counters.cases += 1;
  counters.groups[group] = (counters.groups[group] ?? 0) + 1;

  for (const entry of snapshots) {
    counters.elementsCompared += entry.capture.elements.length;
    counters.screenshots += 1;
  }
}

/** Fails with an itemized list; screenshots must be byte-identical. */
function expectIdentical(reference: Snapshot, actual: Snapshot, group: string) {
  count(group, [reference, actual]);
  expect(describeDiffs(compareCaptures(reference.capture, actual.capture))).toEqual([]);
  expect(actual.png === reference.png, 'screenshots differ (exact PNG comparison)').toBe(true);
}

async function settleArtwork(rendered: NewRendered, scenario: ParityScenario) {
  if (scenario.artwork === 'loaded') {
    await expect.poll(() => rendered.container.querySelector('img')).not.toBeNull();
    await rendered.container.querySelector('img')?.decode();
  }

  await untilStable(rendered);
}

/** Renders the reference for `scenario` at a timeline position and returns its snapshot. */
async function referenceSnapshot(
  scenario: ParityScenario,
  dim: Dimension,
  timeline: Timeline,
): Promise<Snapshot> {
  const reference = track(await renderReference(scenario.name, frameOf(dim)));

  // The legacy measurement must find the scrolling labels the scenarios declare, and only those.
  expect((reference.scrolling ?? 0) > 0).toBe(SCROLLING.includes(scenario.name));

  await untilStable(reference, 3);
  await applyTimeline(reference, timeline);
  const result = await snapshot(reference);

  reference.unmount();
  open.splice(open.indexOf(reference), 1);

  return result;
}

async function newSnapshot(
  scenario: ParityScenario,
  dim: Dimension,
  timeline: Timeline,
): Promise<Snapshot> {
  if (scenario.fadeInitial) {
    freezeClock();
  }

  const rendered = track(
    await renderNew(scenario, frameOf(dim), { settle: !scenario.fadeInitial }),
  );

  if (scenario.fadeInitial) {
    // The Surface fade is JS-driven: time is frozen at 0 so the first frame is the initial one.
    await delay(30);
    await stepClock(0);
    await nextFrame(rendered.document.defaultView as Window);
  } else {
    await settleArtwork(rendered, scenario);
  }

  await applyTimeline(rendered, timeline);
  const result = await snapshot(rendered);

  releaseClock();

  rendered.unmount();
  open.splice(open.indexOf(rendered), 1);

  return result;
}

function timelineFor(scenario: ParityScenario, scroll = 0): Timeline {
  return scenario.fadeInitial ? { kind: 'at', ms: 0, scroll } : { kind: 'settled', scroll };
}

async function compareScenario(
  scenario: ParityScenario,
  dim: Dimension,
  group: string,
  scroll = 0,
) {
  await setViewport(frameOf(dim));
  // The new Bar's JS media query reads the test page, so the page viewport must be the panel
  // viewport and fall on the same side of the legacy `(width <= 2036px)` breakpoint as the frame.
  expect(window.matchMedia(LAPTOP_MEDIA_QUERY).matches).toBe(dim.width <= 2036);
  const reference = await referenceSnapshot(scenario, dim, timelineFor(scenario, scroll));
  const actual = await newSnapshot(scenario, dim, timelineFor(scenario, scroll));

  expectIdentical(reference, actual, group);
}

describe('environment and provenance', () => {
  test('Maple Mono NF Medium is installed and is what renders at 13px/500', () => {
    expect(font.message).toBe('Maple Mono NF Medium verified');
    expect(font.measured?.advanceH).toBeCloseTo(font.measured?.expectedAdvanceH ?? 0, 6);
    expect(font.measured?.distinct).toBe(true);
  });

  test('the device scale matches the project and the colour scheme is dark', () => {
    expect([1, 2]).toContain(SCALE);
    expect(matchMedia('(prefers-color-scheme: dark)').matches).toBe(true);
  });

  test('the legacy reference provenance verifies', async () => {
    const result = await verifyReference(referenceFiles, manifest, { harnessSources });

    expect(result.problems).toEqual([]);
  });

  test('every reference scenario has exactly one parity mapping', () => {
    const names = (manifest as unknown as { scenarios: string[] }).scenarios;

    expect(names).toHaveLength(65);
    expect(scenarios.map((scenario) => scenario.name).sort()).toEqual([...names].sort());
    expect(Object.keys(fixtures).sort()).toEqual([...names].sort());
  });
});

describe.each(DIMENSIONS)(
  'matrix $id (display $display, padding $padding, viewport $width x $height)',
  (dim) => {
    const names = dim.full ? scenarios.map((scenario) => scenario.name) : SUBSET;

    test.each(names)(
      '%s',
      async (name) => {
        await compareScenario(scenarioOf(name), dim, 'scenario');
      },
      TIMEOUT,
    );
  },
);

/** `cubic-bezier(x1, y1, x2, y2)` evaluated by bisection, independent of any browser timing code. */
function bezierOf(easing: string): (progress: number) => number {
  const [x1, y1, x2, y2] = (/cubic-bezier\(([^)]+)\)/u.exec(easing)?.[1] ?? '')
    .split(',')
    .map(Number) as [number, number, number, number];
  const at = (a: number, b: number, t: number) =>
    3 * (1 - t) ** 2 * t * a + 3 * (1 - t) * t ** 2 * b + t ** 3;

  return (progress) => {
    let low = 0;
    let high = 1;

    for (let step = 0; step < 80; step++) {
      const mid = (low + high) / 2;

      if (at(x1, x2, mid) < progress) {
        low = mid;
      } else {
        high = mid;
      }
    }

    return at(y1, y2, (low + high) / 2);
  };
}

function surfaceOpacities(rendered: Rendered): number[] {
  const win = rendered.document.defaultView as Window;

  return [...rendered.container.querySelectorAll('button')].map((button) =>
    Number(win.getComputedStyle(button).opacity),
  );
}

function forget(rendered: Rendered) {
  rendered.unmount();
  open.splice(open.indexOf(rendered), 1);
}

describe('Surface motion fade (legacy motionRaw.durationSlow = 0.275s)', () => {
  test('the legacy easing/duration constants are the ones the reference CSS carries', () => {
    expect(referenceFiles['reference.css']).toContain(
      `${LEGACY_MOTION.easing} ${LEGACY_MOTION.duration}ms`,
    );
    expect(LEGACY_MOTION.surfaceFade).toBe(0.275 * 1000);
  });

  test('the new Surface passes motion exactly the legacy initial/animate/transition values', () => {
    expect(surfaceMotion.initial).toEqual({ opacity: 0 });
    expect(surfaceMotion.animate).toEqual({ opacity: 1 });
    expect(surfaceMotion.transition).toEqual({
      ease: [0.42, 0, 0.58, 1],
      duration: LEGACY_MOTION.surfaceFade / 1000,
    });
  });

  test.each(['bar-default', 'media-paused'])(
    '%s samples follow the legacy curve at t=0, midpoint and end',
    async (name) => {
      const dim = DIMENSIONS[0] as Dimension;
      const model = bezierOf(LEGACY_MOTION.easing);

      await setViewport(frameOf(dim));
      freezeClock();
      const rendered = track(await renderNew(scenarioOf(name), frameOf(dim), { settle: false }));
      const samples: Record<number, number[]> = {};

      // Let React's passive effects start the fade while time is frozen at 0.
      await delay(30);

      // motion evaluates whole milliseconds, so the 137.5ms midpoint is bracketed by 137 and 138.
      for (const ms of [0, 137, 138, LEGACY_MOTION.surfaceFade]) {
        await stepClock(ms);
        samples[ms] = surfaceOpacities(rendered);
        expect(samples[ms]?.length).toBeGreaterThan(0);

        for (const opacity of samples[ms] ?? []) {
          // The ends are exact. Between them motion's own bezier evaluation differs from the exact
          // curve by up to ~2e-4 (it is the same library and configuration the legacy Surface ran).
          expect(Math.abs(opacity - model(ms / LEGACY_MOTION.surfaceFade))).toBeLessThan(5e-4);
        }
      }

      expect(samples[0]?.every((value) => value === 0)).toBe(true);
      expect(samples[LEGACY_MOTION.surfaceFade]?.every((value) => value === 1)).toBe(true);

      // At the end of the fade the whole Bar equals the legacy settled reference, pixel for pixel.
      await nextFrame(rendered.document.defaultView as Window);
      const finished = await snapshot(rendered);

      releaseClock();
      forget(rendered);

      const reference = await referenceSnapshot(scenarioOf(name), dim, { kind: 'settled' });

      expectIdentical(reference, finished, 'fade-end');
    },
    TIMEOUT,
  );
});

interface TransitionInfo {
  property: string;
  duration: string;
  easing: string;
  delay: string;
}

function transitionsOf(rendered: Rendered): TransitionInfo[] {
  const CssTransition = (rendered.document.defaultView as Window & typeof globalThis).CSSTransition;

  return rendered.document
    .getAnimations()
    .filter((animation): animation is CSSTransition => animation instanceof CssTransition)
    .map((animation) => {
      const timing = animation.effect?.getTiming();

      return {
        property: animation.transitionProperty,
        duration: String(timing?.duration),
        easing: String(timing?.easing),
        delay: String(timing?.delay),
      };
    })
    .sort((left: TransitionInfo, right: TransitionInfo) =>
      left.property.localeCompare(right.property),
    );
}

const HIDDEN = 'lr-Bar-barHidden';

interface Slide {
  reference: Snapshot;
  actual: Snapshot;
  referenceTransitions: TransitionInfo[];
  actualTransitions: TransitionInfo[];
}

/** Slides the Bar away (`hide`) or back (`reveal`) and samples the transition at `at`. */
async function slide(
  direction: 'hide' | 'reveal',
  dim: Dimension,
  at: Timeline,
  name = 'media-paused',
): Promise<Slide> {
  await setViewport(frameOf(dim));
  const scenario = scenarioOf(name);
  const reference = track(await renderReference(name, frameOf(dim)));
  const bar = reference.container.querySelector<HTMLElement>('.lr-Bar-bar');

  if (!bar) {
    throw new Error('Reference has no Bar root');
  }

  const win = reference.document.defaultView as Window;

  // Legacy composes `cx(bar, menuHidden && barHidden)`: toggling the class is the whole change.
  if (direction === 'reveal') {
    bar.classList.add(HIDDEN);
  }

  await untilStable(reference, 3);
  await applyTimeline(reference, { kind: 'settled' });
  bar.classList.toggle(HIDDEN);
  void win.getComputedStyle(bar).transform;
  await nextFrame(win);
  const referenceTransitions = transitionsOf(reference);

  await applyTimeline(reference, at);
  const referenceSnap = await snapshot(reference);

  forget(reference);

  const rendered = track(
    await renderNew(
      { ...scenario, menu: direction === 'reveal' ? 'visible' : 'hidden' },
      frameOf(dim),
    ),
  );

  await settleArtwork(rendered, scenario);
  await applyTimeline(rendered, { kind: 'settled' });
  rendered.emitMenu(direction === 'reveal' ? 'hidden' : 'visible');
  await nextFrame(rendered.document.defaultView as Window);
  await nextFrame(rendered.document.defaultView as Window);
  const actualTransitions = transitionsOf(rendered);

  await applyTimeline(rendered, at);
  const actualSnap = await snapshot(rendered);

  forget(rendered);

  return { reference: referenceSnap, actual: actualSnap, referenceTransitions, actualTransitions };
}

const SLIDE_DIMS = [DIMENSIONS[0] as Dimension, DIMENSIONS[1] as Dimension];

describe.each(SLIDE_DIMS)('menu bar slide $id', (dim) => {
  const samples = [
    { label: 't=0', ms: 0 },
    { label: 'midpoint', ms: LEGACY_MOTION.duration / 2 },
    { label: 'end', ms: LEGACY_MOTION.duration },
  ];

  for (const direction of ['hide', 'reveal'] as const) {
    test.each(samples)(
      `${direction} at $label`,
      async ({ ms }) => {
        const result = await slide(direction, dim, { kind: 'at', ms });

        expect(result.referenceTransitions.map((entry) => entry.property)).toEqual([
          'opacity',
          'transform',
        ]);
        expect(result.actualTransitions).toEqual(result.referenceTransitions);
        expect(result.referenceTransitions[0]?.duration).toBe(String(LEGACY_MOTION.duration));
        expectIdentical(result.reference, result.actual, `slide-${direction}`);
      },
      TIMEOUT,
    );
  }
});

describe('reduced motion', () => {
  beforeEach(async () => {
    await commands.parityEmulate('dark', 'reduce');
  });

  afterEach(async () => {
    await commands.parityEmulate('dark', 'no-preference');
  });

  describe.each(SLIDE_DIMS)('$id', (dim) => {
    test.each(SUBSET)(
      '%s',
      async (name) => {
        await compareScenario(scenarioOf(name), dim, 'reduced-motion');
      },
      TIMEOUT,
    );

    test.each(['hide', 'reveal'] as const)(
      'the slide is instant (%s)',
      async (direction) => {
        const result = await slide(direction, dim, { kind: 'at', ms: 0 });

        expect(result.referenceTransitions).toEqual([]);
        expect(result.actualTransitions).toEqual([]);
        expectIdentical(result.reference, result.actual, `reduced-slide-${direction}`);
      },
      TIMEOUT,
    );
  });
});

describe.each(SLIDE_DIMS)('hover and active $id', (dim) => {
  // Media plus the six status items.
  const BUTTONS = 7;

  test.each(['hover', 'active'] as const)(
    'every clickable-looking item, %s',
    async (state) => {
      await setViewport(frameOf(dim));
      const scenario = scenarioOf('media-paused');

      const point = async (rendered: Rendered, index: number): Promise<Snapshot> => {
        await commands.parityPointer('hover', rendered.selector, 'button', index);

        if (state === 'active') {
          await commands.parityPointer('down', '', '', 0);
        }

        await nextFrame(rendered.document.defaultView as Window);
        await applyTimeline(rendered, { kind: 'settled' });
        const result = await snapshot(rendered);

        await commands.parityPointer('reset', '', '', 0);

        return result;
      };

      for (let index = 0; index < BUTTONS; index++) {
        const reference = track(await renderReference(scenario.name, frameOf(dim)));

        await untilStable(reference, 3);
        const referenceSnap = await point(reference, index);

        forget(reference);

        const rendered = track(await renderNew(scenario, frameOf(dim)));

        await settleArtwork(rendered, scenario);
        const actualSnap = await point(rendered, index);

        forget(rendered);

        // Not vacuous: the pointed-at button really is in its legacy hover colours.
        const hovered = referenceSnap.capture.elements.filter(
          (element) => element.tag === 'button',
        )[index];

        expect(hovered?.style['background-color']).toBe('rgb(69, 71, 90)');
        expectIdentical(referenceSnap, actualSnap, `pointer-${state}`);
      }
    },
    TIMEOUT * 3,
  );
});

describe.each(SLIDE_DIMS)('scrolling labels with overflow, mid-cycle $id', (dim) => {
  test.each(SCROLLING)(
    '%s at half of the scroll cycle',
    async (name) => {
      await compareScenario(scenarioOf(name), dim, 'scroll-mid', 0.5);
    },
    TIMEOUT,
  );
});

describe('the render is deterministic (no tolerance is needed)', () => {
  test.each(['bar-default', 'media-paused', 'cpu-threshold-84'])(
    '%s renders to identical pixels twice on each side',
    async (name) => {
      const dim = DIMENSIONS[1] as Dimension;
      const scenario = scenarioOf(name);

      await setViewport(frameOf(dim));
      const references = [
        await referenceSnapshot(scenario, dim, { kind: 'settled' }),
        await referenceSnapshot(scenario, dim, { kind: 'settled' }),
      ];
      const actuals = [
        await newSnapshot(scenario, dim, { kind: 'settled' }),
        await newSnapshot(scenario, dim, { kind: 'settled' }),
      ];

      expect(references[1]?.png === references[0]?.png).toBe(true);
      expect(actuals[1]?.png === actuals[0]?.png).toBe(true);
    },
    TIMEOUT,
  );
});

describe('negative tests: the gate catches regressions in the new render', () => {
  const dim = DIMENSIONS[0] as Dimension;
  const name = 'media-paused';

  interface Perturbation {
    label: string;
    categories: DiffCategory[];
    apply: (rendered: NewRendered) => void;
  }

  const inject = (css: string) => (rendered: NewRendered) => {
    const style = rendered.document.createElement('style');

    style.textContent = css;
    rendered.document.head.append(style);
  };

  const perturbations: Perturbation[] = [
    {
      label: 'font family',
      categories: ['style', 'geometry'],
      apply: inject(':root { font-family: Menlo, monospace !important; }'),
    },
    {
      label: 'font size',
      categories: ['style', 'geometry'],
      apply: inject(':root { font-size: 14px !important; }'),
    },
    {
      label: 'padding +1px',
      categories: ['style', 'geometry'],
      apply: inject(
        '[data-testid="status-container"] > button { padding-right: 11px !important; }',
      ),
    },
    {
      label: 'gap +1px',
      categories: ['style', 'geometry'],
      apply: inject(
        '[data-testid="status-container"] { column-gap: 7px !important; gap: 7px !important; }',
      ),
    },
    {
      label: 'icon path',
      categories: ['svg'],
      apply: (rendered) => {
        const path = rendered.container.querySelector('svg path');

        path?.setAttribute('d', 'M2 2L22 22');
      },
    },
    {
      label: 'media centering (translateX)',
      categories: ['style', 'geometry'],
      apply: inject('[data-testid="media-container"] { transform: translateX(-49%) !important; }'),
    },
    {
      label: 'colour',
      categories: ['style'],
      apply: inject('[data-testid="status-container"] > button { color: #ffffff !important; }'),
    },
  ];

  test.each(perturbations)(
    '$label is reported by the comparison and the screenshot',
    async ({ categories, apply }) => {
      await setViewport(frameOf(dim));
      const scenario = scenarioOf(name);
      const reference = await referenceSnapshot(scenario, dim, { kind: 'settled' });
      const rendered = track(await renderNew(scenario, frameOf(dim)));

      await settleArtwork(rendered, scenario);
      await applyTimeline(rendered, { kind: 'settled' });

      // The unperturbed render is identical, so any difference below comes from the perturbation.
      const clean = await snapshot(rendered);

      expect(compareCaptures(reference.capture, clean.capture)).toEqual([]);
      expect(clean.png === reference.png).toBe(true);

      apply(rendered);
      await nextFrame(rendered.document.defaultView as Window);
      const perturbed = await snapshot(rendered);
      const diffs = compareCaptures(reference.capture, perturbed.capture);

      expect(diffs.length).toBeGreaterThan(0);

      for (const category of categories) {
        expect(
          diffs.some((diff) => diff.category === category),
          `no ${category} difference reported`,
        ).toBe(true);
      }

      expect(perturbed.png === reference.png, 'screenshot comparison did not notice').toBe(false);
      counters.groups['negative'] = (counters.groups['negative'] ?? 0) + 1;
    },
    TIMEOUT,
  );

  describe('provenance', () => {
    const clone = <T,>(value: T): T => structuredClone(value);

    test('a tampered manifest pin fails verifyReference', async () => {
      const tampered = clone(manifest);

      tampered.pinnedCommit = '0'.repeat(40);

      const result = await verifyReference(referenceFiles, tampered, { harnessSources });

      expect(result.ok).toBe(false);
    });

    test('a tampered manifest hash fails verifyReference', async () => {
      const tampered = clone(manifest);
      const output = tampered.outputs[0];

      if (!output) {
        throw new Error('manifest has no outputs');
      }

      output.sha256 = '0'.repeat(64);

      const result = await verifyReference(referenceFiles, tampered, { harnessSources });

      expect(result.ok).toBe(false);
    });

    test('a tampered reference fixture or icon fails verifyReference', async () => {
      const fixtureFile = 'fixtures/fixtures.json';
      const iconFile = Object.keys(referenceFiles).find((file) => file.endsWith('.svg')) as string;

      for (const file of [fixtureFile, iconFile]) {
        const tampered = { ...referenceFiles, [file]: `${referenceFiles[file]} ` };
        const result = await verifyReference(tampered, manifest, { harnessSources });

        expect(result.ok, `${file} tamper went unnoticed`).toBe(false);
      }
    });

    test('a reference harness that imports the new implementation fails verifyReference', async () => {
      const result = await verifyReference(referenceFiles, manifest, {
        harnessSources: {
          ...harnessSources,
          './render.ts': "import { Bar } from '../../src/shell/windows/Bar';",
        },
      });

      expect(result.ok).toBe(false);
    });
  });
});
