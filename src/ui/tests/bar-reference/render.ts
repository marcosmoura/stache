/**
 * WebKit render harness for the Bar legacy reference.
 *
 * Mounts one reference fixture plus `reference.css` into an isolated iframe so the legacy `:root`
 * rules, `(width <= 2036px)` media queries and `position: fixed` media overlay are evaluated
 * against the iframe viewport, never the Vitest page. Animations present at mount are paused at a
 * fixed timeline position. Nothing here imports the new implementation.
 */
import { fixtures, referenceCss, type ReferenceFixture } from './reference.ts';

/** `(width <= 2036px)` from the legacy `LAPTOP_MEDIA_QUERY`. */
export const LAPTOP_MEDIA_QUERY = '(width <= 2036px)';

/** Root font stack the reference must report, as written in the legacy `main.css`. */
export const ROOT_FONT_STACK = "'Maple Mono NF', 'SF Mono', monospace";

export interface MountOptions {
  /** Fixture name from `fixtures/fixtures.json`. */
  fixture: string;
  /** Iframe viewport width in CSS pixels: the Bar panel viewport, not the display width. */
  width: number;
  /** Iframe height in CSS pixels. The legacy panel height is a runner choice; default 40. */
  height?: number;
  /**
   * Expected device pixel ratio. A page cannot change it, so the harness only asserts it; the
   * runner sets the Playwright context scale.
   */
  scale?: number;
  /** Forces the compact/wide markup. By default the iframe viewport's own media query decides. */
  compact?: boolean;
  /** Timeline position (ms) the paused animations are set to. `null` leaves them running. */
  animationTimeMs?: number | null;
}

export interface MountedReference {
  frame: HTMLIFrameElement;
  document: Document;
  /** The legacy `.root` container. */
  root: HTMLElement;
  /** Which stored markup was used. */
  variant: 'wide' | 'compact';
  fixture: ReferenceFixture;
  unmount: () => void;
}

/**
 * Normalizes a computed `font-family` to the form written in the legacy CSS. Browsers differ in
 * whether they quote family names, so names are unquoted, then re-quoted with single quotes when
 * they contain whitespace.
 */
export function normalizeFontStack(value: string): string {
  return value
    .split(',')
    .map((family) => family.trim().replaceAll(/^["']|["']$/gu, ''))
    .map((family) => (/\s/u.test(family) ? `'${family}'` : family))
    .join(', ');
}

function waitForLoad(frame: HTMLIFrameElement): Promise<void> {
  return new Promise((resolve, reject) => {
    frame.addEventListener('load', () => resolve(), { once: true });
    frame.addEventListener('error', () => reject(new Error('Reference frame failed to load')), {
      once: true,
    });
  });
}

export async function mountReference(options: MountOptions): Promise<MountedReference> {
  const fixture = fixtures[options.fixture];

  if (!fixture) {
    throw new Error(`Unknown Bar reference fixture: ${options.fixture}`);
  }

  if (options.scale !== undefined && window.devicePixelRatio !== options.scale) {
    throw new Error(
      `Device scale ${window.devicePixelRatio} != requested ${options.scale}; configure the browser context`,
    );
  }

  const frame = document.createElement('iframe');

  frame.dataset.barReference = options.fixture;
  frame.style.cssText = [
    'position:fixed',
    'top:0',
    'left:0',
    'border:0',
    `width:${options.width}px`,
    `height:${options.height ?? 40}px`,
  ].join(';');
  // The legacy page shell is `html > body > .root` (main.css styles all three).
  frame.srcdoc = `<!doctype html><html><head><meta charset="utf-8"><style>${referenceCss}</style></head><body><div class="root"></div></body></html>`;

  const loaded = waitForLoad(frame);

  document.body.append(frame);
  await loaded;

  const frameDocument = frame.contentDocument;
  const frameWindow = frame.contentWindow;

  if (!frameDocument || !frameWindow) {
    throw new Error('Reference frame has no document');
  }

  const compact = options.compact ?? frameWindow.matchMedia(LAPTOP_MEDIA_QUERY).matches;
  const variant = compact ? 'compact' : 'wide';
  const root = frameDocument.querySelector<HTMLElement>('.root');

  if (!root) {
    throw new Error('Reference frame has no .root');
  }

  root.innerHTML = fixture[variant];
  await frameDocument.fonts.ready;

  if (options.animationTimeMs !== null) {
    for (const animation of frameDocument.getAnimations()) {
      animation.pause();
      animation.currentTime = options.animationTimeMs ?? 0;
    }
  }

  return { frame, document: frameDocument, root, variant, fixture, unmount: () => frame.remove() };
}
