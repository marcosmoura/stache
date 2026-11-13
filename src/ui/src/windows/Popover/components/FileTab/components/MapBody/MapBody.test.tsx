import { describe, expect, it } from 'vitest';
import { userEvent } from 'vitest/browser';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { MapBody } from './MapBody';

const map = node.map(node.string());
const copy = { bindings: { label: 'Bindings', noun: 'Shortcut' } };

describe('MapBody', () => {
  it('lists the entries and Add opens a new row without writing', async () => {
    const { screen, edits } = await renderForm(
      <MapBody node={map} path={['bindings']} at="bindings" depth={1} />,
      { value: { bindings: { 'cmd+a': 'x' } }, copy },
    );

    await expect.element(screen.getByRole('group', { name: 'cmd+a' })).toBeVisible();
    await screen.getByRole('button', { name: 'Add shortcut' }).click();
    await expect.element(screen.getByRole('group', { name: 'New shortcut' })).toBeVisible();
    await expect.element(screen.getByRole('button', { name: 'Add shortcut' })).toBeDisabled();
    expect(edits).toEqual([]);
    await userEvent.keyboard('{Escape}');
    await expect
      .element(screen.getByRole('group', { name: 'New shortcut' }))
      .not.toBeInTheDocument();
  });
});
