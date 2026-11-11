/**
 * Declared stubs for the I/O boundary of the pinned legacy Bar sources.
 *
 * Everything that is presentation (components, styles, state derivation such as labels, colors and
 * icon selection) runs from the pinned sources. These stubs only replace what cannot run here:
 * Tauri/React Query data access, the window `matchMedia` hook, framer-style motion, the Linaria
 * compiler and the Weather store's provider plumbing. Each stub is listed in the manifest.
 */
import * as React from 'react';

export const SUSPEND = Symbol('suspend');
export const FAIL = Symbol('fail');

export interface ScrollState {
  start: number;
  end: number;
  distance: number;
}

export interface WeatherFixture {
  weather?: { currentConditions?: { feelslike: number; icon: string } };
  isLoading: boolean;
  isConfigured: boolean;
}

/** Synthetic Tiling state answered by the `invoke` boundary (`get_tiling_*` commands). */
export interface TilingFixture {
  /** `is_tiling_enabled` / `INITIALIZED`: the hook's `isEnabled` state. */
  enabled: boolean;
  /** Workspace names in the order Tiling reports them (legacy sorts them). */
  workspaces: string[];
  focusedWorkspace: string | null;
  /** Windows of the focused workspace, in Tiling order. */
  windows: { id: number; appName: string; title: string }[];
  /** `id` of the focused window, or `null`. */
  focusedWindow: number | null;
}

/** Mutable per-render input shared by all stubs. */
export interface RenderContext {
  compact: boolean;
  queries: Record<string, unknown>;
  weather: WeatherFixture;
  /** `null`: Tiling is not running, every `get_tiling_*` invoke rejects (legacy: empty defaults). */
  tiling: TilingFixture | null;
  /** Opacity motion has committed on `Surface` (1 once the 0.275s fade has finished). */
  surfaceOpacity: 0 | 1;
  /** `useState` result for the Media artwork preloader; `undefined` keeps the real hook. */
  loadedArtwork?: string | null;
  /** Measurement-derived `ScrollingLabel` state, by label order in the render. */
  scrollStates: (ScrollState | undefined)[];
  scrollCursor: number;
  /** ISO instant the legacy clock formatter runs against. */
  now: string;
}

export interface CssRule {
  module: string;
  name: string;
  className: string;
  text: string;
}

const noop = () => undefined;
const pending = () => new Promise<never>(() => undefined);

function readQuery(context: RenderContext, key: unknown, suspend: boolean): unknown {
  if (!Array.isArray(key) || typeof key[0] !== 'string') {
    throw new Error('Unexpected query key');
  }

  const name = key[0];

  if (!(name in context.queries)) {
    throw new Error(`No fixture data for query "${name}"`);
  }

  const value = context.queries[name];

  if (value === FAIL) {
    throw new Error(`Fixture query "${name}" rejected`);
  }

  if (value === SUSPEND) {
    if (suspend) {
      throw pending();
    }

    return undefined;
  }

  return value;
}

function withFrozenDate<T>(iso: string, run: () => T): T {
  const RealDate = globalThis.Date;
  const fixed = new RealDate(iso).getTime();

  class FrozenDate extends RealDate {
    constructor(...args: unknown[]) {
      super(...((args.length === 0 ? [fixed] : args) as [number]));
    }

    static override now() {
      return fixed;
    }
  }

  globalThis.Date = FrozenDate as DateConstructor;

  try {
    return run();
  } finally {
    globalThis.Date = RealDate;
  }
}

export function createStubs(
  context: RenderContext,
  options: {
    getSource: (modulePath: string) => string;
    cssRules: CssRule[];
    weatherIcons: () => { getWeatherIcon: unknown };
  },
) {
  const makeQueryReader =
    (suspend: boolean) => (queryOptions: { queryKey: unknown; queryFn?: () => unknown }) => {
      const key = queryOptions.queryKey;

      if (Array.isArray(key) && key[0] === 'clock' && queryOptions.queryFn) {
        const queryFn = queryOptions.queryFn;

        return { data: withFrozenDate(context.now, () => queryFn()) };
      }

      return { data: readQuery(context, key, suspend) };
    };

  const invokeFixture = (command: string): Promise<unknown> => {
    const tiling = context.tiling;
    const focused = tiling?.windows.find((window) => window.id === tiling.focusedWindow) ?? null;
    const answers: Record<string, unknown> = tiling
      ? {
          get_tiling_workspaces: tiling.workspaces.map((name) => ({ name })),
          get_tiling_focused_workspace: tiling.focusedWorkspace,
          get_tiling_current_workspace_windows: tiling.windows,
          get_tiling_focused_window: focused,
        }
      : {};

    return command in answers
      ? Promise.resolve(answers[command])
      : Promise.reject(new Error('invoke is not available in the reference'));
  };

  const reactShim = {
    ...React,
    default: React,
    useState(initial: unknown) {
      if (initial === null && context.loadedArtwork !== undefined) {
        return [context.loadedArtwork, noop];
      }

      if (typeof initial === 'object' && initial !== null && 'distance' in initial) {
        const override = context.scrollStates[context.scrollCursor++];

        if (override) {
          return [override, noop];
        }
      }

      return (React.useState as (value: unknown) => unknown)(initial);
    },
  };

  // `useSpaces` keeps `isEnabled` in `useState(false)`, flipped by `is_tiling_enabled` or the
  // `INITIALIZED` event in effects that static rendering never runs: the fixture decides it.
  const spacesStateReactShim = {
    ...reactShim,
    useState(initial: unknown) {
      if (initial === false) {
        return [context.tiling?.enabled ?? false, noop];
      }

      return reactShim.useState(initial);
    },
  };

  /**
   * Settled motion: the `animate` target as inline style (`initial` while the Surface fade is at its
   * first frame). Elements without `animate` (the workspace indicator) keep their own style.
   */
  function settledStyle(
    style: React.CSSProperties | undefined,
    initial: unknown,
    animate: unknown,
  ): React.CSSProperties | undefined {
    if (animate === undefined) {
      return style;
    }

    const target = context.surfaceOpacity === 0 ? initial : animate;

    return { ...style, ...(target as React.CSSProperties) };
  }

  type MotionProps = Record<string, unknown> & { style?: React.CSSProperties };

  const motionlessProps = ({
    initial,
    animate,
    transition: _transition,
    exit: _exit,
    layoutId: _layoutId,
    style,
    ...rest
  }: MotionProps) => ({ ...rest, style: settledStyle(style, initial, animate) });

  const motionProxy = new Proxy(
    {
      create: (Component: React.ElementType) =>
        function MotionWrapped(props: MotionProps) {
          return React.createElement(Component, motionlessProps(props));
        },
    } as Record<string, unknown>,
    {
      get(target, property) {
        if (property in target) {
          return target[property as string];
        }

        if (typeof property !== 'string') {
          return undefined;
        }

        return function MotionElement(props: MotionProps) {
          return React.createElement(property, motionlessProps(props));
        };
      },
    },
  );

  const eventNames = new Proxy(
    {},
    {
      get: (_target, group) =>
        typeof group === 'string'
          ? new Proxy({}, { get: (_t, event) => `${group}.${String(event)}` })
          : undefined,
    },
  );

  return {
    stubs: {
      react: (importer: string) =>
        importer.endsWith('/renderer/bar/Spaces/Spaces.state.ts')
          ? spacesStateReactShim
          : reactShim,
      '@linaria/core': (importer: string) => ({
        cx: (...args: unknown[]) => args.filter((arg) => typeof arg === 'string' && arg).join(' '),
        css: createCss(importer, options.getSource, options.cssRules),
      }),
      '@tanstack/react-query': () => ({
        QueryClient: class QueryClient {
          resetQueries = noop;
        },
        QueryClientProvider: ({ children }: { children: React.ReactNode }) => children,
        useSuspenseQuery: makeQueryReader(true),
        useQuery: makeQueryReader(false),
        useQueryClient: () => ({ setQueryData: noop, invalidateQueries: noop }),
      }),
      'react-error-boundary': () => ({
        ErrorBoundary: ({ children }: { children: React.ReactNode }) => children,
        useErrorBoundary: () => ({ resetBoundary: noop }),
      }),
      '@tauri-apps/api/core': () => ({
        invoke: invokeFixture,
      }),
      '@tauri-apps/api/event': () => ({ listen: () => Promise.resolve(noop) }),
      'motion/react': () => ({
        motion: motionProxy,
        // Settled: presence and layout groups render their children (no enter/exit in progress).
        AnimatePresence: ({ children }: { children: React.ReactNode }) => children,
        LayoutGroup: ({ children }: { children: React.ReactNode }) => children,
      }),
      '@/types': () => eventNames,
      '@/utils/queryClientDefaults': () => ({ queryClientDefaults: {} }),
      '@/hooks': () => ({
        useDisableRightClick: noop,
        useTauriEvent: noop,
        useWidgetToggle: () => ({ ref: null, onClick: noop }),
        useMediaQuery: (query: string) => {
          if (query !== '(width <= 2036px)') {
            throw new Error(`Unexpected media query ${query}`);
          }

          return context.compact;
        },
        useTauri: makeQueryReader(false),
        useTauriSuspense: makeQueryReader(true),
      }),
      '@/stores/WeatherStore': () => ({
        ...options.weatherIcons(),
        useWeatherStore: () => context.weather,
      }),
    } satisfies Record<string, (importer: string) => unknown>,
    pathStubs: {} as Record<string, unknown>,
  };
}

function createCss(importer: string, getSource: (modulePath: string) => string, rules: CssRule[]) {
  const source = getSource(importer);
  const names = [...source.matchAll(/export const (\w+) = css`/g)].map((match) => match[1]);
  const total = (source.match(/\bcss`/g) ?? []).length;

  if (names.length !== total) {
    throw new Error(`${importer}: every css template must be \`export const name = css\``);
  }

  const stem = importer
    .split('/')
    .pop()
    ?.replace(/\.styles\.tsx?$/, '');
  let index = 0;

  return (strings: TemplateStringsArray, ...values: unknown[]) => {
    const name = names[index++];

    if (!name || !stem) {
      throw new Error(`${importer}: unexpected css template #${index}`);
    }

    const text = strings.reduce((output, chunk, position) => {
      if (position === 0) {
        return chunk;
      }

      const value = values[position - 1];

      if (typeof value !== 'string' && typeof value !== 'number') {
        throw new Error(
          `${importer}:${name}: cannot resolve interpolation #${position} (${typeof value})`,
        );
      }

      return `${output}${value}${chunk}`;
    }, '');

    const className = `lr-${stem}-${name}`;

    rules.push({ module: importer, name, className, text });

    return className;
  };
}
