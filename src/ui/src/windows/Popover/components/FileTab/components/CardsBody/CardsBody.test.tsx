import { describe, expect, it } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { CardsBody } from './CardsBody';

const cards = node.array(node.object({ name: node.string() }));

describe('CardsBody', () => {
  it('titles each card and adds one at the end', async () => {
    const { screen, edits } = await renderForm(
      <CardsBody node={cards} path={['workspaces']} at="workspaces" depth={1} />,
      {
        value: { workspaces: [{ name: 'coding' }, {}] },
        copy: { workspaces: { label: 'Workspaces', noun: 'Workspace' } },
      },
    );

    await expect.element(screen.getByRole('group', { name: 'coding' })).toBeVisible();
    await expect.element(screen.getByRole('group', { name: 'Workspace 2' })).toBeVisible();
    await screen.getByRole('button', { name: 'Add workspace' }).click();
    expect(edits).toHaveLength(1);
    expect(edits[0]).toMatchObject({ op: 'insert', path: ['workspaces'], index: 2 });
  });
});
