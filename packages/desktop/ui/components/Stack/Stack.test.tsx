import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { LAPTOP_MEDIA_QUERY } from '@/utils/media-query';

type MockMediaQueryList = MediaQueryList & {
  trigger: (matches: boolean) => void;
};

let Stack: typeof import('./Stack').Stack;
let mediaQueries: Map<string, MockMediaQueryList>;
const initialMatches = new Map<string, boolean>();
let matchMediaMock: typeof window.matchMedia;
const originalMatchMedia = window.matchMedia;

const createMock = (query: string): MockMediaQueryList => {
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  let currentMatches = initialMatches.get(query) ?? false;

  const mql = {
    media: query,
    onchange: null as ((this: MediaQueryList, ev: MediaQueryListEvent) => void) | null,
    addEventListener: (_event: 'change', listener: (event: MediaQueryListEvent) => void) => {
      listeners.add(listener);
    },
    removeEventListener: (_event: 'change', listener: (event: MediaQueryListEvent) => void) => {
      listeners.delete(listener);
    },
    addListener: (listener: (this: MediaQueryList, ev: MediaQueryListEvent) => void) => {
      void listener;
    },
    removeListener: (listener: (this: MediaQueryList, ev: MediaQueryListEvent) => void) => {
      void listener;
    },
    dispatchEvent: () => true,
    trigger: (matches: boolean) => {
      currentMatches = matches;
      const event = { matches, media: query } as MediaQueryListEvent;
      listeners.forEach((listener) => listener(event));
      mql.onchange?.call(mql as unknown as MediaQueryList, event);
    },
  } as const;

  return new Proxy(mql, {
    get(target, property, receiver) {
      if (property === 'matches') {
        return currentMatches;
      }

      return Reflect.get(target, property, receiver);
    },
  }) as unknown as MockMediaQueryList;
};

describe('Stack Component', () => {
  beforeEach(async () => {
    vi.resetModules();
    mediaQueries = new Map<string, MockMediaQueryList>();
    initialMatches.clear();

    matchMediaMock = vi.fn<typeof window.matchMedia>((query: string) => {
      let mql = mediaQueries.get(query);

      if (!mql) {
        mql = createMock(query);
        mediaQueries.set(query, mql);
      }

      return mql;
    });

    window.matchMedia = matchMediaMock;

    const module = await import('./Stack');
    Stack = module.Stack;
  });

  afterEach(() => {
    window.matchMedia = originalMatchMedia;
  });

  test('renders children correctly', async () => {
    const { getByText } = await render(
      <Stack>
        <span>Child 1</span>
        <span>Child 2</span>
      </Stack>,
    );

    await vi.waitFor(() => {
      expect(getByText('Child 1')).toBeDefined();
      expect(getByText('Child 2')).toBeDefined();
    });
  });

  test('renders as div element', async () => {
    const { container } = await render(<Stack>Content</Stack>);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div).toBeDefined();
    });
  });

  test('applies custom className', async () => {
    const customClass = 'my-stack-class';
    const { container } = await render(<Stack className={customClass}>Styled</Stack>);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div?.classList.contains(customClass)).toBe(true);
    });
  });

  test('forwards additional HTML attributes', async () => {
    const { container } = await render(
      <Stack data-testid="test-stack" aria-label="Stack container">
        Children
      </Stack>,
    );

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div?.getAttribute('data-testid')).toBe('test-stack');
      expect(div?.getAttribute('aria-label')).toBe('Stack container');
    });
  });

  test('applies base stack styles', async () => {
    const { container } = await render(<Stack>Base</Stack>);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div?.classList.length).toBeGreaterThanOrEqual(1);
    });
  });

  test('applies compact styles on laptop screen', async () => {
    initialMatches.set(LAPTOP_MEDIA_QUERY, true);
    vi.resetModules();

    const module = await import('./Stack');
    const StackWithLaptop = module.Stack;

    const { container } = await render(<StackWithLaptop>Compact</StackWithLaptop>);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      // Should have base class and compact class
      expect(div?.classList.length).toBeGreaterThanOrEqual(2);
    });
  });

  test('renders correctly with media query', async () => {
    const { container } = await render(<Stack>Test Content</Stack>);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div).toBeDefined();
      // Should have at least the base stack class
      expect(div?.classList.length).toBeGreaterThanOrEqual(1);
    });
  });

  test('handles multiple children', async () => {
    const { getByText } = await render(
      <Stack>
        <span>First</span>
        <span>Second</span>
        <span>Third</span>
      </Stack>,
    );

    await vi.waitFor(() => {
      expect(getByText('First')).toBeDefined();
      expect(getByText('Second')).toBeDefined();
      expect(getByText('Third')).toBeDefined();
    });
  });

  test('handles empty children', async () => {
    const { container } = await render(<Stack />);

    await vi.waitFor(() => {
      const div = container.querySelector('div');
      expect(div).toBeDefined();
      expect(div?.children.length).toBe(0);
    });
  });
});
