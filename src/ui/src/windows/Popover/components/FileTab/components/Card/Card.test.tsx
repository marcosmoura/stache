import { describe, expect, it, vi } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';
import type { ArrayEdits } from '../../utils/arrayEdits';

import { Card } from './Card';

const edits = (): ArrayEdits =>
  ({
    disabled: false,
    insert: vi.fn(),
    move: vi.fn(),
    remove: vi.fn(),
    write: { pending: false, message: null, setMessage: vi.fn(), run: vi.fn() },
  }) as ArrayEdits;

describe('Card', () => {
  it('starts collapsed, opens its fields and moves by keyboard buttons', async () => {
    const actions = edits();
    const { screen } = await renderForm(
      <ul>
        <Card
          node={node.object({ name: node.string() })}
          path={['workspaces', 1]}
          at="workspaces.*"
          index={1}
          count={2}
          title="coding"
          depth={1}
          edits={actions}
        />
      </ul>,
      { value: { workspaces: [{}, { name: 'coding' }] } },
    );
    const toggle = screen.getByRole('button', { name: 'coding', exact: true });

    expect(screen.getByRole('textbox').query()).toBeNull();
    await toggle.click();
    await expect.element(screen.getByRole('textbox', { name: 'Name' })).toHaveValue('coding');
    await expect.element(screen.getByRole('button', { name: 'Move coding down' })).toBeDisabled();
    await screen.getByRole('button', { name: 'Move coding up' }).click();
    expect(actions.move).toHaveBeenCalledWith(1, 0);
    expect(screen.container.querySelector('[data-path="workspaces[1]"]')).not.toBeNull();
  });
});
