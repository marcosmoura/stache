import { describe, expect, it } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { ListBody } from './ListBody';

const list = node.array(node.string());

describe('ListBody', () => {
  it('shows one row per item with move and remove, and adds at the end', async () => {
    const { screen, edits } = await renderForm(
      <ListBody node={list} path={['apps']} at="apps" label="Apps" depth={1} />,
      { value: { apps: ['a', 'b'] }, copy: { apps: { label: 'Apps', noun: 'App' } } },
    );

    await expect.element(screen.getByRole('textbox', { name: 'App 2' })).toHaveValue('b');
    await expect.element(screen.getByRole('button', { name: 'Move App 1 up' })).toBeDisabled();
    await screen.getByRole('button', { name: 'Move App 1 down' }).click();
    await screen.getByRole('button', { name: 'Add app' }).click();
    expect(edits.map((edit) => edit.op)).toEqual(['move', 'insert']);
  });

  it('says Empty without items', async () => {
    const { screen } = await renderForm(
      <ListBody node={list} path={['apps']} at="apps" label="Apps" depth={1} />,
    );

    await expect.element(screen.getByText('Empty')).toBeVisible();
  });
});
