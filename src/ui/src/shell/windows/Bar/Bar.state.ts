import { useLayoutEffect } from 'react';

import { useBarSnapshot, type BarSnapshotOutcome } from '../../../sdk';
import { useDisableRightClick } from '../../hooks';

import { barDataFromOutcome } from './Bar.data';
import type { BarState } from './Bar.types';

/**
 * Legacy `menuHidden` is true while the system menu bar is on screen, which
 * slides the Bar away. Only an `available` view whose `menu.state` is `visible`
 * means that; `hidden`, `unknown`, `unavailable`, `unauthorized` and an unread
 * feed all keep the Bar shown, like legacy's initial `false`.
 */
export function isMenuBarVisible(outcome: BarSnapshotOutcome | undefined): boolean {
  return outcome?.kind === 'available' && outcome.view.menu.state === 'visible';
}

/** Marks the document so the Bar's root rules apply to this window only. */
export function useBarDocument() {
  useLayoutEffect(() => {
    document.documentElement.dataset.surface = 'bar';

    return () => {
      delete document.documentElement.dataset.surface;
    };
  }, []);
}

export function useBar(): BarState {
  const { data } = useBarSnapshot();

  useDisableRightClick();

  return {
    menuHidden: isMenuBarVisible(data),
    data: barDataFromOutcome(data),
    configRevision: data?.kind === 'available' ? data.view.configRevision : null,
    mediaArtworkId: data?.kind === 'available' ? (data.view.media?.artworkId ?? null) : null,
    weatherEpoch: data?.kind === 'available' ? data.view.weatherEpoch : '0',
  };
}
