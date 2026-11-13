import { describe, expect, it } from 'vitest';

import { render } from '../../../../../tests/render';

import { GuideSteps } from './GuideSteps';

describe('GuideSteps', () => {
  it('numbers the three steps in order', async () => {
    const screen = await render(<GuideSteps />);
    const items = screen.getByRole('listitem').elements();

    expect(items.map((item) => item.textContent)).toEqual([
      '1Click Request Access, or Open System Settings.',
      '2In Privacy & Security > Accessibility, turn on Stache.',
      '3Come back here; Stache starts by itself.',
    ]);
    await expect.poll(() => getComputedStyle(items[0]!).display).toBe('flex');
  });
});
