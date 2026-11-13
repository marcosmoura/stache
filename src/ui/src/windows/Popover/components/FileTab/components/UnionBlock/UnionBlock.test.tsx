import { describe, expect, it } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { UnionBlock } from './UnionBlock';

const union = node.union({
  text: node.string(),
  table: node.object({ size: node.number() }),
});

describe('UnionBlock', () => {
  it('shows the kind menu in its header and the active form below', async () => {
    const { screen } = await renderForm(
      <UnionBlock node={union} path={['icon']} at="icon" label="Icon" inTable depth={1} />,
      {
        value: { icon: 'cpu' },
        copy: {
          icon: {
            label: 'Icon',
            valueLabel: 'Name',
            variants: { text: { label: 'Text' }, table: { label: 'Table' } },
          },
        },
      },
    );

    await expect
      .element(screen.getByRole('button', { name: 'Icon kind' }))
      .toHaveTextContent('Text');
    await expect.element(screen.getByRole('textbox', { name: 'Name' })).toHaveValue('cpu');
  });

  it('switches the form by writing its seed', async () => {
    const { screen, edits } = await renderForm(
      <UnionBlock
        node={union}
        path={['icon']}
        at="icon"
        label="Icon"
        inTable={false}
        depth={1}
        bare
      />,
      { value: { icon: 'cpu' } },
    );

    await expect.element(screen.getByText('Type')).toBeVisible();
    await screen.getByRole('button', { name: 'Icon kind' }).click();
    await screen.getByRole('option', { name: /^Table/ }).click();
    expect(edits).toHaveLength(1);
    expect(edits[0]).toMatchObject({ op: 'set', path: ['icon'] });
  });
});
