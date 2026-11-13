import { describe, expect, it } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { OptionalBlock } from './OptionalBlock';

const optional = node.optional(node.object({ size: node.number() }));

describe('OptionalBlock', () => {
  it('shows Not set with Set, which writes the seed', async () => {
    const { screen, edits } = await renderForm(
      <OptionalBlock node={optional} path={['extra']} at="extra" label="Extra" inTable depth={1} />,
    );

    await expect.element(screen.getByText('Not set')).toBeVisible();
    await screen.getByText('Set', { exact: true }).click();
    expect(edits).toHaveLength(1);
    expect(edits[0]).toMatchObject({ op: 'set', path: ['extra'] });
  });

  it('renders the inner field once set', async () => {
    const { screen } = await renderForm(
      <OptionalBlock node={optional} path={['extra']} at="extra" label="Extra" inTable depth={1} />,
      { value: { extra: { size: 3 } } },
    );

    await expect.element(screen.getByRole('region', { name: 'Extra' })).toBeVisible();
    await expect.element(screen.getByRole('textbox', { name: 'Size' })).toHaveValue('3');
  });
});
