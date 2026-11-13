import type { FileCopy, OptionCopy } from './types';

/** Animation curves (Tiling animations, border animations and fades). */
export const EASING: Record<string, OptionCopy> = {
  linear: { label: 'Linear', hint: 'Constant speed' },
  ease_in: { label: 'Ease in', hint: 'Starts slow' },
  ease_out: { label: 'Ease out', hint: 'Ends slow' },
  ease_in_out: { label: 'Ease in and out', hint: 'Starts and ends slow' },
  ease_out_expo: { label: 'Quick ease out', hint: 'Very fast start, gentle stop' },
  spring: { label: 'Spring', hint: 'Bouncy, physical motion' },
};

/** Fields under `prefix` with `prefix.` prepended to each path. */
export const under = (prefix: string, copy: FileCopy): FileCopy =>
  Object.fromEntries(
    Object.entries(copy).map(([path, field]) => [path ? `${prefix}.${path}` : prefix, field]),
  );

/** A rule that matches windows by app or title (ignore lists, workspace rules). */
export const windowRule: FileCopy = {
  '': { label: 'Rule', order: ['app_name', 'title', 'app_id'] },
  app_id: { label: 'Bundle ID', hint: 'Exact app identifier, such as com.apple.Safari.' },
  app_name: { label: 'App name', hint: 'Matches part of the app name.' },
  title: { label: 'Window title', hint: 'Matches part of the window title.' },
};
