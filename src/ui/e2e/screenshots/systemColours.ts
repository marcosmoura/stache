import type { Page } from '@playwright/test';

import { expect } from '../fixtures';

/** The system colours that follow user settings, pinned to macOS defaults. */
export const PINNED: Readonly<Record<string, string>> = {
  '-apple-system-control-accent': 'rgb(0 122 255)',
  '-apple-system-selected-content-background': 'rgb(0 99 225)',
  '-webkit-focus-ring-color': 'rgb(0 103 244)',
};

/** Neutral stand-ins for the native `.popover` material behind a transparent native-look page. */
export const MATERIAL = { dark: 'rgb(44 44 46)', light: 'rgb(236 236 236)' } as const;

/**
 * The harness's stable fallbacks: overrides each StyleX variable whose value
 * is a user-dependent system colour keyword with its default, on the root
 * element (inline styles win over the `:root` declarations), and paints a
 * neutral stand-in for the native material behind the transparent page.
 */
export async function pinSystemColours(page: Page, scheme: keyof typeof MATERIAL) {
  const count = await page.evaluate(
    ([pinned, material]) => {
      let pinnedCount = 0;

      document.documentElement.style.background = material;

      // StyleX nests its rules in `@layer` blocks.
      const walk = (rules: CSSRuleList) => {
        for (const rule of rules) {
          if (rule instanceof CSSGroupingRule) {
            walk(rule.cssRules);
          } else if (rule instanceof CSSStyleRule) {
            for (const name of rule.style) {
              const value = pinned[rule.style.getPropertyValue(name).trim()];

              if (name.startsWith('--') && value !== undefined) {
                document.documentElement.style.setProperty(name, value);
                pinnedCount += 1;
              }
            }
          }
        }
      };

      for (const sheet of document.styleSheets) {
        walk(sheet.cssRules);
      }

      return pinnedCount;
    },
    [PINNED, MATERIAL[scheme]] as const,
  );

  expect(count, 'the native tokens were found and pinned').toBeGreaterThan(0);
}
