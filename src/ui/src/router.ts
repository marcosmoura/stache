import { lazy, type ComponentType } from 'react';

/** Every window surface the host opens, keyed by its `?surface=` value. */
export type Surface = 'access' | 'bar' | 'popover';

/** Props every routed window receives. */
export type WindowProps = { surface: Surface };

/** Props of the local fallback rendered for an empty or unknown surface. */
export type FallbackProps = { requested: string };

const AccessibilityGuide = lazy(() =>
  import('@/windows/AccessibilityGuide').then((module) => ({ default: module.AccessibilityGuide })),
);

const Popover = lazy(() =>
  import('@/windows/Popover').then((module) => ({ default: module.Popover })),
);

const Bar = lazy(() => import('@/windows/Bar').then((module) => ({ default: module.Bar })));

const UnknownSurface = lazy(() =>
  import('@/windows/UnknownSurface').then((module) => ({ default: module.UnknownSurface })),
);

/** The surface table. Each entry is a lazily loaded window, so each window is its own chunk. */
export const surfaces: Readonly<Record<Surface, ComponentType<WindowProps>>> = {
  access: AccessibilityGuide,
  bar: Bar,
  popover: Popover,
};

export type Route =
  | { kind: 'window'; surface: Surface; Window: ComponentType<WindowProps> }
  | { kind: 'fallback'; requested: string; Window: ComponentType<FallbackProps> };

const isSurface = (value: string): value is Surface => Object.hasOwn(surfaces, value);

/**
 * Resolves a location search string to a route. There is no default window: an
 * absent, empty or unknown value (matched case-sensitively) selects the local
 * fallback, which calls no native command. When the parameter repeats, the
 * first value wins.
 */
export function resolveRoute(search: string): Route {
  const value = new URLSearchParams(search).get('surface') ?? '';

  if (isSurface(value)) {
    return { kind: 'window', surface: value, Window: surfaces[value] };
  }

  return { kind: 'fallback', requested: value, Window: UnknownSurface };
}
