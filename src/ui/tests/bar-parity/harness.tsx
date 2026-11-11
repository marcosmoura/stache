import { flushSync } from 'react-dom';
import { createRoot } from 'react-dom/client';

/**
 * Renders the legacy reference and the new Bar into identical iframes and freezes them at a known
 * timeline position, so `capture.ts` and screenshots compare like with like.
 *
 * Both sides live in a fixed, borderless iframe of exactly the panel viewport (`display - 2 *
 * padding` wide, the Bar height tall). The new Bar is rendered by the test page's React into that
 * frame's `#root`, inside the document shell production serves (`index.html` + `main.css`, the
 * `data-surface` marker its own hook sets, and the real Linaria/Bar stylesheets).
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { vi } from 'vitest';
import { commands, page } from 'vitest/browser';

import mainCss from '../../main.css?raw';
import type { BarChanged } from '../../src/sdk/bindings.gen';
import { Bar } from '../../src/shell/windows/Bar';
import type { BarData } from '../../src/shell/windows/Bar/Bar.data';
import { BarDataProvider } from '../../src/shell/windows/Bar/BarDataProvider';
import { mountReference, type MountedReference } from '../bar-reference/render';
import { availableOutcome } from '../barFixtures';

import { captureDocument, type Capture } from './capture';
import type { ParityScenario } from './scenarios';

export interface FrameConfig {
  /** Panel viewport width: `display - 2 * padding`, or the breakpoint width itself. */
  width: number;
  /** Bar (panel) height. */
  height: number;
}

export type Timeline =
  /** Every finite animation/transition is finished; the scrolling label sits at its start. */
  | { kind: 'settled'; scroll?: number }
  /** Every finite animation/transition is paused at `ms`. */
  | { kind: 'at'; ms: number; scroll?: number };

export interface Rendered {
  /** Reference only: how many labels the legacy measurement turned into scrolling labels. */
  scrolling?: number;
  /** `data-parity` value of the iframe, usable as a CSS selector. */
  selector: string;
  frame: HTMLIFrameElement;
  document: Document;
  /** The element holding the Bar (`.root` or `#root`). */
  container: HTMLElement;
  unmount: () => void;
}

export interface NewRendered extends Rendered {
  /** Emits a `bar-changed` event moving the menu bar to `state`. */
  emitMenu: (state: 'hidden' | 'visible') => void;
  /** Re-renders the same Bar with new fixture data (a later Tiling state, for transitions). */
  rerender: (data: BarData) => void;
}

const frames = () => document.querySelectorAll('iframe[data-parity]');

export async function setViewport(config: FrameConfig) {
  // Room under the Bar for the pointer to rest outside the frame.
  await page.viewport(config.width, config.height + 40);
}

export function nextFrame(win: Window = window): Promise<void> {
  return new Promise((resolve) => win.requestAnimationFrame(() => resolve()));
}

async function frames_(win: Window, count: number) {
  for (let i = 0; i < count; i++) {
    await nextFrame(win);
  }
}

/** Waits until the serialized subtree stops changing for `quiet` consecutive frames. */
export async function untilStable(rendered: Rendered, quiet = 8, limit = 90) {
  const win = rendered.document.defaultView as Window;
  let previous = '';
  let same = 0;

  for (let i = 0; i < limit && same < quiet; i++) {
    await nextFrame(win);
    const current = rendered.container.innerHTML;

    same = current === previous ? same + 1 : 0;
    previous = current;
  }

  await rendered.document.fonts.ready;
}

/** Pauses/finishes animations in the frame; scrolling labels are pinned at `scroll` of one cycle. */
export async function applyTimeline(rendered: Rendered, timeline: Timeline) {
  const win = rendered.document.defaultView as Window;

  for (const animation of rendered.document.getAnimations()) {
    const timing = animation.effect?.getComputedTiming();

    if (timing?.iterations === Infinity) {
      const cycle = Number(timing.duration);

      animation.pause();
      animation.currentTime = cycle * (timeline.scroll ?? 0);
      continue;
    }

    if (timeline.kind === 'settled') {
      animation.finish();
    } else {
      animation.pause();
      animation.currentTime = timeline.ms;
    }
  }

  await frames_(win, 2);
}

function releaseFrame(frame: HTMLIFrameElement) {
  frame.remove();
}

function frameCss(config: FrameConfig) {
  return [
    'position:fixed',
    'top:0',
    'left:0',
    'border:0',
    `width:${config.width}px`,
    `height:${config.height}px`,
  ].join(';');
}

/** The legacy reference, mounted through its own harness (`bar-reference/render.ts`). */
export async function renderReference(
  fixture: string,
  config: FrameConfig,
  options: { compact?: boolean } = {},
): Promise<Rendered> {
  const mounted: MountedReference = await mountReference({
    fixture,
    width: config.width,
    height: config.height,
    scale: window.devicePixelRatio,
    animationTimeMs: null,
    ...options,
  });

  mounted.frame.dataset.parity = 'reference';
  // The reference's page shell omits `<html lang="en">` that the legacy index.html
  // (_legacy/app/ui/index.html:2 at e0b4648) and the new index.html both have; computed
  // `-webkit-locale` follows it. Mirrored here so only the Bar differs.
  mounted.document.documentElement.lang = 'en';

  const rendered: Rendered = {
    selector: 'iframe[data-parity="reference"]',
    frame: mounted.frame,
    document: mounted.document,
    container: mounted.root,
    unmount: mounted.unmount,
  };

  splitLegacyInterpolatedText(rendered);
  rendered.scrolling = applyLegacyScrollMeasurement(rendered);

  return rendered;
}

/**
 * Applies the legacy `ScrollingLabel` algorithm to every scrolling label of the reference.
 *
 * Legacy measures the plain label (`wrapper.offsetWidth`, `label.scrollWidth`); when it overflows it
 * switches to the scrolling classes and its ResizeObserver measures again. The reference ships the
 * result of that with fixed numbers (and one pre-measurement frame, `media-long-title`), so the
 * state is recomputed on the reference's own layout with the legacy formula (`start = wrapper
 * width`, `end = -label scrollWidth`, `duration = max(1, 1 + distance / 60)`), in the same two
 * steps. Labels that do not overflow stay as shipped. Nothing is read from the new Bar.
 */
export function applyLegacyScrollMeasurement(rendered: Rendered) {
  const wrapperClass = 'lr-ScrollingLabel-scrollingWrapper';
  const labelClass = 'lr-ScrollingLabel-scrollingLabel';
  let scrolling = 0;

  for (const wrapper of rendered.container.querySelectorAll<HTMLElement>(
    '.lr-ScrollingLabel-wrapper',
  )) {
    const label = wrapper.querySelector<HTMLElement>('.lr-ScrollingLabel-label');

    if (!label) {
      throw new Error('Scrolling wrapper without label');
    }

    const measure = (): boolean => {
      const width = wrapper.offsetWidth;
      const labelWidth = label.scrollWidth;

      if (labelWidth - width <= 0) {
        return false;
      }

      label.style.setProperty('--scroll-start', `${width}px`);
      label.style.setProperty('--scroll-end', `${-labelWidth}px`);
      label.style.setProperty(
        '--scroll-duration',
        `${Math.max(1, 1 + (width + labelWidth) / 60)}s`,
      );

      return true;
    };

    // Step 1: plain classes.
    wrapper.classList.remove(wrapperClass);
    label.classList.remove(labelClass);

    if (!measure()) {
      // Legacy resets the state to { start: 0, end: 0, distance: 0 } when nothing overflows.
      label.style.setProperty('--scroll-start', '0px');
      label.style.setProperty('--scroll-end', '0px');
      label.style.setProperty('--scroll-duration', '1s');
      continue;
    }

    // Step 2: scrolling classes applied, the ResizeObserver measures again.
    wrapper.classList.add(wrapperClass);
    label.classList.add(labelClass);
    measure();
    scrolling += 1;
  }

  return scrolling;
}

/**
 * The legacy Cpu renders `<span>{usage}%</span>` and `<span>{temperature}°C</span>`
 * (_legacy/app/ui/renderer/bar/Status/Cpu/Cpu.tsx:12-13 at e0b4648). In the client DOM React gives
 * each interpolation its own text node ("23" + "%"); the reference ships static markup with one
 * node, which rasterizes the `%` with a different subpixel origin (2 pixels at the wide layout).
 * The reference DOM is split the way the legacy JSX builds it. Text and markup are unchanged.
 */
export function splitLegacyInterpolatedText(rendered: Rendered) {
  const buttons = [
    ...rendered.container.querySelectorAll<HTMLElement>(
      '[data-testid="status-container"] > button',
    ),
  ];
  const hasWeather = buttons[0]?.querySelector('.lr-Weather-label') !== null;
  const cpu = buttons[hasWeather ? 1 : 0];

  if (!cpu) {
    return;
  }

  for (const span of cpu.querySelectorAll('span')) {
    const match = /^(.*?)(%|°C)$/u.exec(span.textContent ?? '');

    if (match?.[1] !== undefined && span.childNodes.length === 1) {
      span.replaceChildren(
        rendered.document.createTextNode(match[1]),
        rendered.document.createTextNode(match[2] ?? ''),
      );
    }
  }
}

function projectStyles(): string {
  const css: string[] = [];

  for (const style of document.querySelectorAll<HTMLStyleElement>('style[data-vite-dev-id]')) {
    const id = style.dataset.viteDevId ?? '';

    if (/\/src\/ui\/src\//u.test(id)) {
      css.push(style.textContent ?? '');
    }
  }

  return css.join('\n');
}

const handlers: { change?: (event: { payload: BarChanged }) => void } = {};

function mockTauri(menu: 'hidden' | 'visible') {
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
  vi.mocked(listen).mockImplementation((async (
    _event: string,
    handler: (event: { payload: BarChanged }) => void,
  ) => {
    handlers.change = handler;

    return () => {};
  }) as never);
  vi.mocked(invoke).mockImplementation((async (command: string) => {
    if (command === 'bar_snapshot') {
      return availableOutcome({ sequence: '1', menu: { state: menu, reason: null } });
    }

    throw new Error(`Unexpected command: ${command}`);
  }) as never);
}

/** The new Bar through the static fixture seam, inside the production document shell. */
export async function renderNew(
  scenario: ParityScenario,
  config: FrameConfig,
  options: { settle?: boolean } = {},
): Promise<NewRendered> {
  mockTauri(scenario.menu);

  const frame = document.createElement('iframe');

  frame.dataset.parity = 'new';
  frame.style.cssText = frameCss(config);
  frame.srcdoc = `<!doctype html><html lang="en"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width, initial-scale=1"><style>${mainCss}</style><style>${projectStyles()}</style></head><body><div id="root"></div></body></html>`;

  const loaded = new Promise<void>((resolve) =>
    frame.addEventListener('load', () => resolve(), { once: true }),
  );

  document.body.append(frame);
  await loaded;

  const frameDocument = frame.contentDocument;
  const container = frameDocument?.getElementById('root');

  if (!frameDocument || !container) {
    throw new Error('New Bar frame has no #root');
  }

  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  });
  const root = createRoot(container);
  const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

  const draw = (data: BarData) => {
    flushSync(() => {
      root.render(
        <QueryClientProvider client={client}>
          <BarDataProvider data={data}>
            <Bar />
          </BarDataProvider>
        </QueryClientProvider>,
      );
    });
  };

  draw(scenario.data);

  // The Bar's own hook marks the document it runs in (here the test page). Production runs in the
  // frame's document, so mirror the marker the hook set rather than hard-coding it.
  const surface = document.documentElement.dataset.surface;

  if (surface !== 'bar') {
    throw new Error(`Bar did not set the document surface marker (got ${String(surface)})`);
  }

  frameDocument.documentElement.dataset.surface = surface;

  const rendered: NewRendered = {
    selector: 'iframe[data-parity="new"]',
    frame,
    document: frameDocument,
    container,
    rerender: draw,
    emitMenu: (state) => {
      const handler = handlers.change;

      if (!handler) {
        throw new Error('bar-changed listener was not registered');
      }

      handler({
        payload: {
          generation: '1',
          sequence: '2',
          resnapshot: false,
          outcome: availableOutcome({ sequence: '2', menu: { state, reason: null } }),
        },
      });
    },
    unmount: () => {
      root.unmount();
      client.clear();
      consoleError.mockRestore();
      releaseFrame(frame);
    },
  };

  if (options.settle !== false) {
    await untilStable(rendered);
  }

  return rendered;
}

export async function screenshot(rendered: Rendered): Promise<string> {
  return commands.parityScreenshot(rendered.selector);
}

export interface Snapshot {
  capture: Capture;
  png: string;
}

export async function snapshot(rendered: Rendered): Promise<Snapshot> {
  return { capture: captureDocument(rendered.document), png: await screenshot(rendered) };
}

export function leftoverFrames(): number {
  return frames().length;
}
