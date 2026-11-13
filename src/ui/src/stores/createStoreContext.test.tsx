import { Profiler } from 'react';

import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-react';
import { useShallow } from 'zustand/react/shallow';

import { createStoreContext } from './createStoreContext';

type Counter = {
  count: number;
  label: string;
  actions: { increment: () => void; rename: (label: string) => void };
};

const counter = createStoreContext<Counter>(
  (set) => ({
    count: 0,
    label: 'counter',
    actions: {
      increment: () => set((state) => ({ count: state.count + 1 })),
      rename: (label) => set({ label }),
    },
  }),
  'CounterStore',
);

function Count({ name }: { name: string }) {
  const count = counter.useStore((state) => state.count);
  const { increment } = counter.useStore((state) => state.actions);

  return (
    <button type="button" onClick={increment}>
      {name} {count}
    </button>
  );
}

describe('createStoreContext', () => {
  it('gives each provider its own store', async () => {
    const screen = await render(
      <>
        <counter.Provider>
          <Count name="a" />
        </counter.Provider>
        <counter.Provider>
          <Count name="b" />
        </counter.Provider>
      </>,
    );

    await screen.getByRole('button', { name: 'a 0' }).click();
    await screen.getByRole('button', { name: 'a 1' }).click();
    await expect.element(screen.getByRole('button', { name: 'a 2' })).toBeVisible();
    await expect.element(screen.getByRole('button', { name: 'b 0' })).toBeVisible();
  });

  it('throws a clear error outside a provider', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    await expect(render(<Count name="orphan" />)).rejects.toThrow(
      'CounterStore: useStore/useStoreApi must be used inside its <Provider>.',
    );
    consoleError.mockRestore();
  });

  it('does not loop with an object selector under useShallow', async () => {
    const renders = { count: 0 };

    function Summary() {
      const { count, label } = counter.useStore(
        useShallow((state) => ({ count: state.count, label: state.label })),
      );

      return (
        <p>
          {label}: {count}
        </p>
      );
    }

    function Controls() {
      const api = counter.useStoreApi();

      return (
        <button type="button" onClick={() => api.getState().actions.rename('renamed')}>
          rename
        </button>
      );
    }

    const screen = await render(
      <counter.Provider>
        <Profiler
          id="summary"
          onRender={() => {
            renders.count += 1;
          }}
        >
          <Summary />
        </Profiler>
        <Controls />
      </counter.Provider>,
    );

    await expect.element(screen.getByText('counter: 0')).toBeVisible();
    const before = renders.count;

    await screen.getByRole('button', { name: 'rename' }).click();
    await expect.element(screen.getByText('renamed: 0')).toBeVisible();
    expect(renders.count).toBe(before + 1);
  });
});
