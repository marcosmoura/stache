import { describe, expect, it } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { UnknownRow } from './UnknownRow';

describe('UnknownRow', () => {
  it('shows the label and the read-only value from the file', async () => {
    const { screen } = await renderForm(
      <UnknownRow node={node.unknown()} path={['odd']} at="odd" label="Odd" inTable depth={1} />,
      {
        value: { odd: [1, 2] },
        copy: { odd: { label: 'Odd', hint: 'Edit it in a text editor.' } },
      },
    );

    await expect.element(screen.getByText('Odd', { exact: true })).toBeVisible();
    await expect.element(screen.getByText('[1, 2]')).toBeVisible();
    await expect.element(screen.getByText('Edit it in a text editor.')).toBeVisible();
  });
});
