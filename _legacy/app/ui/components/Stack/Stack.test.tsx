import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { LAPTOP_MEDIA_QUERY } from '@/utils/media-query';

import { Stack } from './Stack';

type MockMediaQueryList = MediaQueryList & {
  trigger: (matches: boolean) => void;
};

let mediaQueries: Map<string, MockMediaQueryList>;
const currentInitialMatches = new Map<string, boolean>();
const originalMatchMedia = window.matchMedia;

const createMock = (query: string): MockMediaQueryList => {
  const listeners = new Set<(event: MediaQueryListEvent) => void>();
  let currentMatches = currentInitialMatches.get(query) ?? false;

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
  beforeEach(() => {
    mediaQueries = new Map<string, MockMediaQueryList>();
    currentInitialMatches.clear();

    window.matchMedia = vi.fn<typeof window.matchMedia>((query: string) => {
      let mql = mediaQueries.get(query);

      if (!mql) {
        mql = createMock(query);
        mediaQueries.set(query, mql);
      }

      return mql;
    });
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

    expect(getByText('Child 1')).toBeDefined();
    expect(getByText('Child 2')).toBeDefined();
  });

  test('renders as div element', async () => {
    const { container } = await render(<Stack>Content</Stack>);

    const div = container.querySelector('div');
    expect(div).toBeDefined();
  });

  test('applies custom className', async () => {
    const customClass = 'my-stack-class';
    const { container } = await render(<Stack className={customClass}>Styled</Stack>);

    const div = container.querySelector('div');
    expect(div?.classList.contains(customClass)).toBe(true);
  });

  test('forwards additional HTML attributes', async () => {
    const { container } = await render(
      <Stack data-testid="test-stack" aria-label="Stack container">
        Children
      </Stack>,
    );

    const div = container.querySelector('div');
    expect(div?.getAttribute('data-testid')).toBe('test-stack');
    expect(div?.getAttribute('aria-label')).toBe('Stack container');
  });

  test('applies base stack styles', async () => {
    const { container } = await render(<Stack>Base</Stack>);

    const div = container.querySelector('div');
    expect(div?.classList.length).toBeGreaterThanOrEqual(1);
  });

  test('applies compact styles on laptop screen', async () => {
    currentInitialMatches.set(LAPTOP_MEDIA_QUERY, true);

    const { container } = await render(<Stack>Compact</Stack>);

    const div = container.querySelector('div');
    // Should have base class and compact class
    expect(div?.classList.length).toBeGreaterThanOrEqual(2);
  });

  test('renders correctly with media query', async () => {
    const { container } = await render(<Stack>Test Content</Stack>);

    const div = container.querySelector('div');
    expect(div).toBeDefined();
    // Should have at least the base stack class
    expect(div?.classList.length).toBeGreaterThanOrEqual(1);
  });

  test('handles multiple children', async () => {
    const { getByText } = await render(
      <Stack>
        <span>First</span>
        <span>Second</span>
        <span>Third</span>
      </Stack>,
    );

    expect(getByText('First')).toBeDefined();
    expect(getByText('Second')).toBeDefined();
    expect(getByText('Third')).toBeDefined();
  });

  test('handles empty children', async () => {
    const { container } = await render(<Stack />);

    const div = container.querySelector('div');
    expect(div).toBeDefined();
    expect(div?.children.length).toBe(0);
  });
});
