import { afterEach, describe, expect, it } from 'vitest';
import { commands } from 'vitest/browser';

import { recordIpc } from '../../../tests/ipc';
import { render } from '../../../tests/render';

import { UnknownSurface } from './UnknownSurface';

afterEach(async () => {
  await commands.emulateMedia({ colorScheme: null });
});

describe('UnknownSurface', () => {
  it('names the unknown value and calls no command', async () => {
    const calls = recordIpc();
    const screen = await render(<UnknownSurface requested="nope" />);

    await expect.element(screen.getByRole('heading')).toHaveTextContent('Unknown surface');
    await expect.element(screen.getByText('“nope” is not a surface.')).toBeVisible();
    expect(calls).toEqual([]);
  });

  it('explains an empty value and follows a change', async () => {
    const screen = await render(<UnknownSurface requested="x" />);

    await screen.rerender(<UnknownSurface requested="x" />);
    await screen.rerender(<UnknownSurface requested="" />);

    await expect.element(screen.getByText('No surface was requested.')).toBeVisible();
  });

  it('uses the text token, so the fallback is readable in dark mode', async () => {
    await commands.emulateMedia({ colorScheme: 'dark' });

    const screen = await render(<UnknownSurface requested="nope" />);
    const element = screen.getByRole('main').element();

    // Mocha text (#cdd6f4), not the default black.
    await expect.poll(() => getComputedStyle(element).color).toBe('rgb(205, 214, 244)');
    await commands.emulateMedia({ colorScheme: 'light' });
    // Latte text (#4c4f69).
    await expect.poll(() => getComputedStyle(element).color).toBe('rgb(76, 79, 105)');
  });
});
