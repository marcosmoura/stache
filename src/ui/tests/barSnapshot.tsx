import { useState, type ReactNode } from 'react';

import { useQueryClient, type QueryClient } from '@tanstack/react-query';

import type { BarSnapshotOutcome, BarView_Serialize } from '@/ipc';
import { barSnapshotOptions } from '@/queries/bar';

import { barAvailable } from './fixtures';
import { render } from './render';

/**
 * Renders `ui` inside the app providers with the Bar snapshot query seeded to
 * `outcome` (nothing when `undefined`), the way `barFeed` writes it.
 * `setSnapshot` replaces it, as a newer `bar-changed` would; `setView` is a
 * shorthand for an `available` outcome with the given readings.
 */
export async function renderWithSnapshot(ui: ReactNode, outcome?: BarSnapshotOutcome) {
  const holder: { client: QueryClient | null } = { client: null };
  const key = barSnapshotOptions().queryKey;

  function Seed({ children }: { children: ReactNode }) {
    const client = useQueryClient();

    useState(() => {
      holder.client = client;

      if (outcome) {
        client.setQueryData(key, outcome);
      }

      return null;
    });

    return children;
  }

  const screen = await render(<Seed>{ui}</Seed>);
  let sequence = 100;

  const setSnapshot = (next: BarSnapshotOutcome) => {
    holder.client!.setQueryData(key, next);
  };

  const setView = (overrides: Partial<BarView_Serialize>) => {
    sequence += 1;
    setSnapshot(barAvailable(String(sequence), overrides));
  };

  /** Re-renders `next` in place, keeping the seeded client and the instance. */
  const rerender = (next: ReactNode) => screen.rerender(<Seed>{next}</Seed>);

  return { screen, setSnapshot, setView, rerender };
}
