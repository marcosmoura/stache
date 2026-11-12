import { QueryClientProvider, useQuery } from '@tanstack/react-query';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import type { BarSnapshotOutcome } from '@/ipc';

import { barAvailable, barChanged, barUnauthorized, barUnavailable } from '../../tests/fixtures';
import { mockHost } from '../../tests/ipc';
import { createQueryClient } from '../../tests/queryClient';

import {
  barFeed,
  barOutcomeSequence,
  barSnapshotOptions,
  selectBarKind,
  selectBarLayout,
  selectBarView,
} from './bar';
import { useFeed } from './feed';

const selectHeight = (outcome: BarSnapshotOutcome) => selectBarView(outcome)?.height ?? null;
const selectPadding = (outcome: BarSnapshotOutcome) => selectBarView(outcome)?.padding ?? null;

describe('bar queries', () => {
  it('builds the snapshot options', () => {
    expect(barSnapshotOptions().queryKey).toEqual(['bar', 'snapshot']);
    expect(barFeed.queryKey).toEqual(['bar', 'snapshot']);
  });

  it('reads every outcome', () => {
    const available = barAvailable('4', { height: 30 });
    const unavailable = barUnavailable('5');

    expect(barOutcomeSequence(available)).toBe('4');
    expect(barOutcomeSequence(unavailable)).toBe('5');
    expect(barOutcomeSequence(barUnauthorized)).toBeNull();
    expect(selectBarKind(available)).toBe('available');
    expect(selectBarKind(unavailable)).toBe('unavailable');
    expect(selectBarKind(barUnauthorized)).toBe('unauthorized');
    expect(selectBarView(available)?.height).toBe(30);
    expect(selectBarView(unavailable)).toBeNull();
    expect(selectBarView(barUnauthorized)).toBeNull();
    expect(selectBarLayout(available)).toEqual({
      layout: available.kind === 'available' ? available.view.layout : null,
      height: 30,
    });
    expect(selectBarLayout(unavailable)).toBeNull();
    expect(selectBarLayout(barUnauthorized)).toBeNull();
  });

  it('caches the whole outcome, including unavailable', async () => {
    const host = mockHost();

    host.answer('bar_snapshot', barUnavailable('2'));
    const client = createQueryClient();
    const stop = barFeed.start(client);

    await vi.waitFor(() =>
      expect(client.getQueryData(barSnapshotOptions().queryKey)).toEqual(barUnavailable('2')),
    );
    stop();
  });

  it('re-renders a slice subscriber only when its own slice changes', async () => {
    const host = mockHost();

    host.answer('bar_snapshot', barAvailable('1', { height: 30, padding: 4 }));
    const client = createQueryClient();
    const renders = { height: 0, padding: 0 };

    function Height() {
      useFeed(barFeed);
      const { data } = useQuery({ ...barSnapshotOptions(), select: selectHeight });

      renders.height += 1;

      return <span>height {data ?? '-'}</span>;
    }

    function Padding() {
      useFeed(barFeed);
      const { data } = useQuery({ ...barSnapshotOptions(), select: selectPadding });

      renders.padding += 1;

      return <span>padding {data ?? '-'}</span>;
    }

    const screen = await render(
      <QueryClientProvider client={client}>
        <Height />
        <Padding />
      </QueryClientProvider>,
    );

    await expect.element(screen.getByText('height 30')).toBeVisible();
    await expect.element(screen.getByText('padding 4')).toBeVisible();
    const before = { ...renders };

    host.emit('bar-changed', barChanged('2', barAvailable('2', { height: 40, padding: 4 })));
    await expect.element(screen.getByText('height 40')).toBeVisible();
    expect(renders.height).toBe(before.height + 1);
    expect(renders.padding).toBe(before.padding);

    host.emit('bar-changed', barChanged('3', barAvailable('3', { height: 40, padding: 6 })));
    await expect.element(screen.getByText('padding 6')).toBeVisible();
    expect(renders.height).toBe(before.height + 1);
    expect(renders.padding).toBe(before.padding + 1);
  });
});
