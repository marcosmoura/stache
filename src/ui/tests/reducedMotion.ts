import { afterAll, beforeAll } from 'vitest';

/*
 * Motion reads `prefers-reduced-motion` once, when the first motion component
 * mounts in a page, and afterwards only through that query's `change` event.
 * WebKit does not deliver Playwright's emulated changes to that listener, so a
 * test file hands Motion a query it can drive: `setReducedMotion` flips the
 * value and fires `change`, exactly what the system setting does.
 */
const reducedMotionQuery = Object.assign(new EventTarget(), {
  matches: false,
  media: '(prefers-reduced-motion)',
});

/** Sets the controllable `prefers-reduced-motion` value and notifies listeners. */
export function setReducedMotion(reduce: boolean) {
  reducedMotionQuery.matches = reduce;
  reducedMotionQuery.dispatchEvent(new Event('change'));
}

/**
 * Installs the controllable query for the calling test file. Call it at the
 * top level, before anything renders a motion component.
 */
export function installReducedMotionQuery() {
  const realMatchMedia = window.matchMedia.bind(window);

  beforeAll(() => {
    window.matchMedia = (query: string) =>
      query === reducedMotionQuery.media
        ? (reducedMotionQuery as unknown as MediaQueryList)
        : realMatchMedia(query);
  });

  afterAll(() => {
    window.matchMedia = realMatchMedia;
  });
}
