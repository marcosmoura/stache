import { describe, expect, it } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { TableBlock } from './TableBlock';

describe('TableBlock', () => {
  it('wraps a list in a section with its count', async () => {
    const { screen } = await renderForm(
      <TableBlock
        node={node.array(node.string())}
        path={['ignore']}
        at="ignore"
        label="Ignored apps"
        inTable
        depth={1}
      />,
      { value: { ignore: ['a', 'b'] }, copy: { ignore: { label: 'Ignored apps', noun: 'App' } } },
    );
    const section = screen.getByRole('region', { name: 'Ignored apps' });

    await expect.element(section).toBeVisible();
    await expect.element(section.getByText('2', { exact: true })).toBeVisible();
    await expect.element(screen.getByRole('textbox', { name: 'App 1' })).toHaveValue('a');
  });

  it('renders only the body when bare', async () => {
    const { screen } = await renderForm(
      <TableBlock
        node={node.array(node.string())}
        path={['ignore']}
        at="ignore"
        label="Ignored apps"
        inTable={false}
        depth={1}
        bare
      />,
      { value: { ignore: ['a'] } },
    );

    expect(screen.getByRole('region').query()).toBeNull();
    await expect.element(screen.getByRole('textbox', { name: 'Ignored apps 1' })).toHaveValue('a');
  });
});
