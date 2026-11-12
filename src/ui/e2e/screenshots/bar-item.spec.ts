import { expect, test } from '../fixtures';

import { skipWithoutMapleMono } from './font';

/*
 * `BarItem` baselines (overview decision 9) from the harness gallery: rest,
 * hover and active in dark and light. Compared only with 'Maple Mono NF'
 * installed. Update with
 * `bunx playwright test --update-snapshots src/ui/e2e/screenshots/bar-item.spec.ts`.
 */

test.use({ viewport: { width: 320, height: 160 }, deviceScaleFactor: 2 });

for (const scheme of ['dark', 'light'] as const) {
  for (const look of ['rest', 'hover', 'active'] as const) {
    test(`BarItem ${look} (${scheme})`, async ({ page }) => {
      await page.emulateMedia({ colorScheme: scheme });
      await page.goto('/e2e/harness.html?gallery=bar-item');
      await skipWithoutMapleMono(page);

      const shot = page.getByTestId(look === 'active' ? 'shot-active' : 'shot-rest');
      const button = shot.getByRole('button');

      await expect(button).toHaveCSS('opacity', '1');
      await page.evaluate(() => document.fonts.ready.then(() => undefined));

      if (look === 'hover') {
        await button.hover();
      } else {
        await page.mouse.move(0, 0);
      }

      await expect(shot).toHaveScreenshot(`bar-item-${look}-${scheme}.png`);
    });
  }
}
