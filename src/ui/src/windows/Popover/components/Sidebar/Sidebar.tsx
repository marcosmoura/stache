import type { KeyboardEvent } from 'react';

import * as stylex from '@stylexjs/stylex';

import { Icon } from '@/components/Icon';
import { BADGE_TEXT, PANEL_ID, tabId, type PopoverTab } from '@/windows/Popover/utils/tabs';

import { styles } from './Sidebar.style';

export type SidebarProps = {
  tabs: readonly PopoverTab[];
  selected: string;
  onSelect: (id: string) => void;
};

const STEPS: Readonly<Record<string, (index: number, count: number) => number>> = {
  ArrowDown: (index, count) => (index + 1) % count,
  ArrowUp: (index, count) => (index - 1 + count) % count,
  Home: () => 0,
  End: (_index, count) => count - 1,
};

const nameOf = (tab: PopoverTab) =>
  tab.badge === null ? tab.label : `${tab.label}, ${BADGE_TEXT[tab.badge]}`;

/**
 * The popover's narrow icon sidebar: a vertical tab list. Only the selected
 * tab is in the Tab order; ↑/↓ (wrapping), Home and End select and focus
 * another tab. A badge marks a file that is invalid, has a diagnostic or has
 * warnings, and is part of the tab's accessible name and tooltip.
 */
export function Sidebar({ tabs, selected, onSelect }: SidebarProps) {
  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const step = STEPS[event.key];

    if (step === undefined || tabs.length === 0) {
      return;
    }

    event.preventDefault();
    const index = Math.max(
      0,
      tabs.findIndex((tab) => tab.id === selected),
    );
    const next = tabs[step(index, tabs.length)];

    if (next) {
      onSelect(next.id);
      document.getElementById(tabId(next.id))?.focus();
    }
  }

  return (
    <nav {...stylex.props(styles.sidebar)} aria-label="Sections">
      <div
        {...stylex.props(styles.list)}
        role="tablist"
        aria-orientation="vertical"
        onKeyDown={onKeyDown}
      >
        {tabs.map((tab) => {
          const active = tab.id === selected;

          return (
            <button
              key={tab.id}
              id={tabId(tab.id)}
              type="button"
              role="tab"
              aria-selected={active}
              aria-controls={PANEL_ID}
              aria-label={nameOf(tab)}
              title={nameOf(tab)}
              tabIndex={active ? 0 : -1}
              data-badge={tab.badge ?? undefined}
              {...stylex.props(styles.tab, active && styles.selected)}
              onClick={() => onSelect(tab.id)}
            >
              <Icon icon={tab.icon} size={18} strokeWidth={1.75} />
              {tab.badge === null ? null : (
                <span
                  {...stylex.props(
                    styles.badge,
                    tab.badge === 'invalid' && styles.invalid,
                    tab.badge === 'diagnostic' && styles.diagnostic,
                    tab.badge === 'warnings' && styles.warnings,
                  )}
                  aria-hidden="true"
                />
              )}
            </button>
          );
        })}
      </div>
    </nav>
  );
}
