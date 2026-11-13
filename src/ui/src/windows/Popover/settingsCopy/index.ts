import { bar } from './bar';
import { commandQuit } from './commandQuit';
import { global } from './global';
import { keepAwake } from './keepAwake';
import { keybindings } from './keybindings';
import { menuAnywhere } from './menuAnywhere';
import { notunes } from './notunes';
import { proxyAudio } from './proxyAudio';
import { tiling } from './tiling';
import type { FieldCopy, FileCopy, OptionCopy } from './types';
import { wallpapers } from './wallpapers';

export type { FieldCopy, FileCopy, OptionCopy, Unit } from './types';

/**
 * The words of the settings form, by catalog file. Schema descriptions are
 * developer documentation and are never shown; every label, hint, unit and
 * option name comes from here. `copy.test.ts` keeps it complete.
 */
export const COPY: Record<string, FileCopy> = {
  'global.toml': global,
  'bar.toml': bar,
  'commandQuit.toml': commandQuit,
  'keepAwake.toml': keepAwake,
  'keybindings.toml': keybindings,
  'menuAnywhere.toml': menuAnywhere,
  'notunes.toml': notunes,
  'proxyAudio.toml': proxyAudio,
  'tiling.toml': tiling,
  'wallpapers.toml': wallpapers,
};

/** A key as a label: `default_layout` → `Default layout`. */
export function humanise(key: string): string {
  const words = key
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .replace(/[_-]+/g, ' ')
    .trim()
    .toLowerCase();

  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** The copy path of a child: a key, or `*` for an array element or map entry. */
export const childPath = (at: string, key: string) => (at ? `${at}.${key}` : key);

/** The copy at `at`, else a humanised key with no hint. */
export function fieldCopy(copy: FileCopy, at: string): FieldCopy {
  return copy[at] ?? { label: humanise(at.split('.').at(-1) ?? at) };
}

/** An enum value's or a union form's copy, else its humanised value. */
export const optionCopy = (choices: Record<string, OptionCopy> | undefined, value: string) =>
  choices?.[value] ?? { label: humanise(value) };
