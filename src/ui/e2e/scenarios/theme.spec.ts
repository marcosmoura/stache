import { expect, test } from '../fixtures';

/* Catppuccin Latte and Mocha `base` (surface) and `text`, from the published palette. */
const latte = { surface: 'rgb(239, 241, 245)', text: 'rgb(76, 79, 105)' };
const mocha = { surface: 'rgb(30, 30, 46)', text: 'rgb(205, 214, 244)' };

test.describe('light and dark themes', () => {
  for (const [scheme, colors] of [
    ['light', latte],
    ['dark', mocha],
  ] as const) {
    test(`prefers-color-scheme: ${scheme} shows the ${scheme === 'light' ? 'Latte' : 'Mocha'} colors`, async ({
      page,
      host,
      consoleErrors,
    }) => {
      await page.emulateMedia({ colorScheme: scheme });
      await page.goto('/?surface=shell');

      const main = page.getByRole('main');

      await expect(main).toHaveCSS('background-color', colors.surface);
      await expect(main).toHaveCSS('color', colors.text);
      expect(host.calls).toEqual([]);
      expect(consoleErrors).toEqual([]);
    });
  }

  test('switches when the system scheme changes, without a reload', async ({ page }) => {
    await page.emulateMedia({ colorScheme: 'light' });
    await page.goto('/?surface=shell');

    const main = page.getByRole('main');

    await expect(main).toHaveCSS('background-color', latte.surface);
    await page.evaluate(() => {
      (window as unknown as { marker: string }).marker = 'same document';
    });

    await page.emulateMedia({ colorScheme: 'dark' });
    await expect(main).toHaveCSS('background-color', mocha.surface);
    await expect(main).toHaveCSS('color', mocha.text);
    expect(await page.evaluate(() => (window as unknown as { marker?: string }).marker)).toBe(
      'same document',
    );

    await page.emulateMedia({ colorScheme: 'light' });
    await expect(main).toHaveCSS('background-color', latte.surface);
  });

  test('the root declares color-scheme: light dark', async ({ page }) => {
    await page.goto('/?surface=shell');

    expect(await page.evaluate(() => getComputedStyle(document.documentElement).colorScheme)).toBe(
      'light dark',
    );
  });
});
