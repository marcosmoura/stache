import { StrictMode } from 'react';

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { PNG_PIXEL } from '../../../../../tests/barFixtures';

import { useMediaArtwork } from './Media.artwork';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const ID_A = 'a'.repeat(64);
const ID_B = 'b'.repeat(64);

function Probe({ id }: { id: string | null }) {
  const artwork = useMediaArtwork(id);

  return <output data-testid="art">{artwork ?? 'none'}</output>;
}

function client() {
  return new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
}

async function mount(id: string | null, strict = false) {
  const queryClient = client();
  const tree = (value: string | null) => {
    const node = (
      <QueryClientProvider client={queryClient}>
        <Probe id={value} />
      </QueryClientProvider>
    );

    return strict ? <StrictMode>{node}</StrictMode> : node;
  };
  const screen = await render(tree(id));

  return { screen, rerender: (value: string | null) => screen.rerender(tree(value)) };
}

const text = (container: HTMLElement) =>
  container.querySelector('[data-testid="art"]')?.textContent;

beforeEach(() => {
  vi.mocked(invoke).mockReset();
});

describe('useMediaArtwork', () => {
  test('fetches by id and returns the base64 once ready', async () => {
    vi.mocked(invoke).mockResolvedValue({ kind: 'ready', artwork: PNG_PIXEL });

    const { screen } = await mount(ID_A);

    await expect.poll(() => text(screen.container)).toBe(PNG_PIXEL);
    expect(invoke).toHaveBeenCalledWith('bar_media_artwork', { id: ID_A });
  });

  test('a null id calls nothing', async () => {
    const { screen } = await mount(null);

    expect(text(screen.container)).toBe('none');
    expect(invoke).not.toHaveBeenCalled();
  });

  test('the same id is fetched once, a new id fetches again', async () => {
    vi.mocked(invoke).mockResolvedValue({ kind: 'ready', artwork: PNG_PIXEL });

    const { screen, rerender } = await mount(ID_A);

    await expect.poll(() => text(screen.container)).toBe(PNG_PIXEL);
    await rerender(ID_A);
    await rerender(null);
    await rerender(ID_A);
    expect(invoke).toHaveBeenCalledTimes(1);

    await rerender(ID_B);
    await expect.poll(() => vi.mocked(invoke).mock.calls.length).toBe(2);
    expect(invoke).toHaveBeenLastCalledWith('bar_media_artwork', { id: ID_B });
  });

  test.each([
    ['missing', { kind: 'missing' }],
    ['unavailable', { kind: 'unavailable', reason: 'notRunning', detail: null }],
    ['unauthorized', { kind: 'unauthorized' }],
  ])('%s yields no artwork', async (_name, outcome) => {
    vi.mocked(invoke).mockResolvedValue(outcome);

    const { screen } = await mount(ID_A);

    await expect.poll(() => vi.mocked(invoke).mock.calls.length).toBe(1);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(text(screen.container)).toBe('none');
  });

  test('a rejection yields no artwork and is not retried', async () => {
    vi.mocked(invoke).mockRejectedValue(new Error('ipc down'));

    const { screen } = await mount(ID_A);

    await expect.poll(() => vi.mocked(invoke).mock.calls.length).toBe(1);
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(text(screen.container)).toBe('none');
  });

  test('a pending newer id never shows the older answer for the newer id', async () => {
    let resolveA: (value: unknown) => void = () => {};

    vi.mocked(invoke).mockImplementation((async (_command: string, args: { id: string }) =>
      args.id === ID_A
        ? new Promise((resolve) => {
            resolveA = resolve;
          })
        : { kind: 'ready', artwork: 'NEWER' }) as never);

    const { screen, rerender } = await mount(ID_A);

    await rerender(ID_B);
    await expect.poll(() => text(screen.container)).toBe('NEWER');
    resolveA({ kind: 'ready', artwork: 'OLDER' });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(text(screen.container)).toBe('NEWER');
  });

  test('survives StrictMode double mounting with one call per id', async () => {
    vi.mocked(invoke).mockResolvedValue({ kind: 'ready', artwork: PNG_PIXEL });

    const { screen } = await mount(ID_A, true);

    await expect.poll(() => text(screen.container)).toBe(PNG_PIXEL);
    expect(invoke).toHaveBeenCalledTimes(1);
    await screen.unmount();
  });
});
