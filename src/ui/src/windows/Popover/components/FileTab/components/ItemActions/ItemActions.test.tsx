import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import type { ArrayEdits } from '../../utils/arrayEdits';

import { ItemActions } from './ItemActions';

describe('ItemActions', () => {
  it('moves and removes, with no move past either end', async () => {
    const edits = { disabled: false, move: vi.fn(), remove: vi.fn() } as unknown as ArrayEdits;
    const screen = await render(<ItemActions title="Rule 1" index={0} count={1} edits={edits} />);

    await expect.element(screen.getByRole('button', { name: 'Move Rule 1 up' })).toBeDisabled();
    await expect.element(screen.getByRole('button', { name: 'Move Rule 1 down' })).toBeDisabled();
    await screen.getByRole('button', { name: 'Remove Rule 1' }).click();
    expect(edits.remove).toHaveBeenCalledWith(0);
  });
});
