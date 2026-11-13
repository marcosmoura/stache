import {
  CommandIcon,
  Coffee02Icon,
  DashboardSquare02Icon,
  File01Icon,
  GridViewIcon,
  HeadphonesIcon,
  Image01Icon,
  KeyboardIcon,
  LayoutTopIcon,
  Menu01Icon,
  MusicNote01Icon,
  Settings02Icon,
} from '@hugeicons/core-free-icons';

import type { IconSource } from '@/components/Icon';
import type { ConfigFileView } from '@/ipc';
import { OVERVIEW_TAB } from '@/stores';

/** Why a file tab carries a badge, most severe first. */
export type BadgeKind = 'invalid' | 'diagnostic' | 'warnings';

export type PopoverTab = {
  /** `overview` or the catalog file name. */
  id: string;
  /** Visible title and accessible name. */
  label: string;
  icon: IconSource;
  badge: BadgeKind | null;
};

/** The words a badge adds to its tab's accessible name and tooltip. */
export const BADGE_TEXT: Readonly<Record<BadgeKind, string>> = {
  invalid: 'invalid',
  diagnostic: 'has a diagnostic',
  warnings: 'has warnings',
};

/** The sidebar icon of each catalog file (also a module row's tile). */
export const FILE_ICONS: Readonly<Record<string, IconSource>> = {
  'global.toml': Settings02Icon,
  'bar.toml': LayoutTopIcon,
  'commandQuit.toml': CommandIcon,
  'keepAwake.toml': Coffee02Icon,
  'keybindings.toml': KeyboardIcon,
  'menuAnywhere.toml': Menu01Icon,
  'notunes.toml': MusicNote01Icon,
  'proxyAudio.toml': HeadphonesIcon,
  'tiling.toml': GridViewIcon,
  'wallpapers.toml': Image01Icon,
};

/** The badge of one file: invalid bytes, else a held diagnostic, else warnings. */
export function badgeOf(file: ConfigFileView): BadgeKind | null {
  if (file.state === 'invalid') {
    return 'invalid';
  }

  if (file.diagnostic != null) {
    return 'diagnostic';
  }

  return file.warnings.length > 0 ? 'warnings' : null;
}

/** Overview first, then one tab per catalog file in the host's order. */
export function tabsFor(files: readonly ConfigFileView[]): PopoverTab[] {
  return [
    { id: OVERVIEW_TAB, label: 'Overview', icon: DashboardSquare02Icon, badge: null },
    ...files.map((file) => ({
      id: file.file,
      label: file.file,
      icon: FILE_ICONS[file.file] ?? File01Icon,
      badge: badgeOf(file),
    })),
  ];
}

/** The DOM id of a tab button (its panel refers to it). */
export const tabId = (id: string) => `popover-tab-${id}`;

/** The DOM id of the tab panel. */
export const PANEL_ID = 'popover-panel';
