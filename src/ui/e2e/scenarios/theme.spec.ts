import { expect, test } from '../fixtures';

/*
 * Catppuccin Latte and Mocha `text`, from the published palette, read from the
 * local fallback (no surface), which renders without IPC.
 */
const latte = { text: 'rgb(76, 79, 105)' };
const mocha = { text: 'rgb(205, 214, 244)' };

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
      await page.goto('/');

      const main = page.getByRole('main');

      await expect(main).toHaveCSS('color', colors.text);
      expect(host.calls).toEqual([]);
      expect(consoleErrors).toEqual([]);
    });
  }

  test('switches when the system scheme changes, without a reload', async ({ page }) => {
    await page.emulateMedia({ colorScheme: 'light' });
    await page.goto('/');

    const main = page.getByRole('main');

    await expect(main).toHaveCSS('color', latte.text);
    await page.evaluate(() => {
      (window as unknown as { marker: string }).marker = 'same document';
    });

    await page.emulateMedia({ colorScheme: 'dark' });
    await expect(main).toHaveCSS('color', mocha.text);
    expect(await page.evaluate(() => (window as unknown as { marker?: string }).marker)).toBe(
      'same document',
    );

    await page.emulateMedia({ colorScheme: 'light' });
    await expect(main).toHaveCSS('color', latte.text);
  });

  test('the root declares color-scheme: light dark', async ({ page }) => {
    await page.goto('/');

    expect(await page.evaluate(() => getComputedStyle(document.documentElement).colorScheme)).toBe(
      'light dark',
    );
  });
});
