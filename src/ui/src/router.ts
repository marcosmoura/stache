import { lazy, type ComponentType } from 'react';

/** Every window surface the host opens, keyed by its `?surface=` value. */
export type Surface = 'shell' | 'test' | 'access' | 'bar';

/** Props every routed window receives. */
export type WindowProps = { surface: Surface };

/** Props of the local fallback rendered for an empty or unknown surface. */
export type FallbackProps = { requested: string };

const Placeholder = lazy(() =>
  import('@/windows/Placeholder').then((module) => ({ default: module.Placeholder })),
);

const AccessibilityGuide = lazy(() =>
  import('@/windows/AccessibilityGuide').then((module) => ({ default: module.AccessibilityGuide })),
);

const TestWindow = lazy(() =>
  import('@/windows/TestWindow').then((module) => ({ default: module.TestWindow })),
);

const Bar = lazy(() => import('@/windows/Bar').then((module) => ({ default: module.Bar })));

const UnknownSurface = lazy(() =>
  import('@/windows/UnknownSurface').then((module) => ({ default: module.UnknownSurface })),
);

/** The surface table. Each entry is a lazily loaded window, so each window is its own chunk. */
export const surfaces: Readonly<Record<Surface, ComponentType<WindowProps>>> = {
  shell: Placeholder,
  test: TestWindow,
  access: AccessibilityGuide,
  bar: Bar,
};

/** The surface used when the `surface` parameter is absent. */
export const DEFAULT_SURFACE: Surface = 'shell';

export type Route =
  | { kind: 'window'; surface: Surface; Window: ComponentType<WindowProps> }
  | { kind: 'fallback'; requested: string; Window: ComponentType<FallbackProps> };

const isSurface = (value: string): value is Surface => Object.hasOwn(surfaces, value);

/**
 * Resolves a location search string to a route. An absent `surface` selects
 * {@link DEFAULT_SURFACE}; an empty or unknown value (matched case-sensitively)
 * selects the local fallback, which calls no native command. When the parameter
 * repeats, the first value wins.
 */
export function resolveRoute(search: string): Route {
  const value = new URLSearchParams(search).get('surface');

  if (value === null) {
    return { kind: 'window', surface: DEFAULT_SURFACE, Window: surfaces[DEFAULT_SURFACE] };
  }

  if (isSurface(value)) {
    return { kind: 'window', surface: value, Window: surfaces[value] };
  }

  return { kind: 'fallback', requested: value, Window: UnknownSurface };
}
