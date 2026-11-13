import { useEffect } from 'react';

import { CommandIcon } from '@hugeicons/core-free-icons';
import * as stylex from '@stylexjs/stylex';
import { useQuery, useQueryClient } from '@tanstack/react-query';

import { Icon } from '@/components/Icon';
import { events, subscribe } from '@/ipc';
import { configFilesOptions, useHidePopover } from '@/queries/popover';
import { OVERVIEW_TAB, popoverStore } from '@/stores';

import { FileTab } from './components/FileTab';
import { OverviewTab } from './components/OverviewTab';
import { Sidebar } from './components/Sidebar';
import { styles } from './Popover.style';
import { PANEL_ID, tabId, tabsFor } from './utils/tabs';

/**
 * The tray popover (`?surface=popover`): a frosted panel with the "Stache"
 * header, the icon sidebar (Overview, then one tab per catalog file) and the
 * selected tab's pane. The selected tab lives in a scoped store, so it
 * survives hiding. Esc hides the popover unless a child already handled it
 * (called `preventDefault`).
 */
export function Popover() {
  return (
    <popoverStore.Provider>
      <PopoverShell />
    </popoverStore.Provider>
  );
}

function PopoverShell() {
  const client = useQueryClient();
  const store = popoverStore.useStoreApi();
  const { data: files } = useQuery(configFilesOptions());
  const selected = popoverStore.useStore((state) => state.tab);
  const { select } = popoverStore.useStore((state) => state.actions);
  const { mutate: hidePopover } = useHidePopover();
  const tabs = tabsFor(files ?? []);
  const current = tabs.find((tab) => tab.id === selected) ?? tabs[0];

  useEffect(() => {
    const refetch = () => {
      void client.invalidateQueries({ queryKey: configFilesOptions().queryKey });
    };
    const stops = [
      subscribe(events.configChanged, refetch),
      subscribe(events.statusChanged, refetch),
      subscribe(events.popoverShown, () => {
        refetch();
        document.getElementById(tabId(store.getState().tab))?.focus();
      }),
    ];

    return () => stops.forEach((stop) => stop());
  }, [client, store]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape' && !event.repeat && !event.defaultPrevented) {
        hidePopover();
      }
    }

    window.addEventListener('keydown', onKeyDown);

    return () => window.removeEventListener('keydown', onKeyDown);
  }, [hidePopover]);

  return (
    <main {...stylex.props(styles.panel)} data-surface="popover" data-tab={current?.id}>
      <header {...stylex.props(styles.header)}>
        <span {...stylex.props(styles.glyph)}>
          <Icon icon={CommandIcon} size={13} strokeWidth={1.75} />
        </span>
        <h1 {...stylex.props(styles.title)}>Stache</h1>
      </header>
      <div {...stylex.props(styles.body)}>
        <Sidebar tabs={tabs} selected={current?.id ?? selected} onSelect={select} />
        <section
          id={PANEL_ID}
          role="tabpanel"
          aria-labelledby={current ? tabId(current.id) : undefined}
          tabIndex={0}
          {...stylex.props(styles.pane)}
        >
          <h2 {...stylex.props(styles.paneTitle)}>{current?.label}</h2>
          {current?.id === OVERVIEW_TAB ? (
            <OverviewTab files={files ?? []} onSelectTab={select} />
          ) : current ? (
            <FileTab key={current.id} file={current.id} />
          ) : null}
        </section>
      </div>
    </main>
  );
}
