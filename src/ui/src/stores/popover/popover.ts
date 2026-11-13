import { createStoreContext } from '../createStoreContext';

/** The Overview tab's id; every other tab is a catalog file name. */
export const OVERVIEW_TAB = 'overview';

export type PopoverState = {
  /** The selected sidebar tab, kept for the session (the popover is only hidden). */
  tab: string;
  actions: { select: (tab: string) => void };
};

/** The popover's scoped store: one per mounted popover. */
export const popoverStore = createStoreContext<PopoverState>(
  (set) => ({
    tab: OVERVIEW_TAB,
    actions: { select: (tab) => set({ tab }) },
  }),
  'PopoverStore',
);
