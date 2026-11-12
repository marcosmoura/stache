import type { Page } from '@playwright/test';

import { test } from '../fixtures';

export const MAPLE_MONO_MISSING =
  "font 'Maple Mono NF' is not installed: screenshot baseline skipped";

/**
 * Whether the system font 'Maple Mono NF' is installed in the page: text set in
 * it must measure differently from both generic fallbacks. Measured in the DOM,
 * since WebKit's canvas does not see user-installed fonts.
 */
export function hasMapleMono(page: Page): Promise<boolean> {
  return page.evaluate(() => {
    const probe = document.createElement('span');

    probe.textContent = 'iiiiiiiiiiWWWWWWWWWW';
    probe.style.position = 'absolute';
    probe.style.whiteSpace = 'nowrap';
    probe.style.visibility = 'hidden';
    document.body.append(probe);

    const width = (family: string) => {
      probe.style.font = `13px ${family}`;

      return probe.getBoundingClientRect().width;
    };

    try {
      return ['serif', 'sans-serif'].every(
        (fallback) => width(`'Maple Mono NF', ${fallback}`) !== width(fallback),
      );
    } finally {
      probe.remove();
    }
  });
}

/**
 * Screenshot baselines are compared only with 'Maple Mono NF' installed;
 * otherwise the test reports an explicit skip, never a silent pass. Call after
 * the page has loaded.
 */
export async function skipWithoutMapleMono(page: Page) {
  test.skip(!(await hasMapleMono(page)), MAPLE_MONO_MISSING);
}
