import { describe, expect, it } from 'vitest';
import { userEvent } from 'vitest/browser';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { MapEntry } from './MapEntry';

const map = node.map(node.string());

const entry = (
  <ul>
    <MapEntry
      node={map}
      path={['bindings']}
      at="bindings.*"
      entryKey="cmd+a"
      keys={['cmd+a', 'cmd+b']}
      noun="Shortcut"
      depth={1}
    />
  </ul>
);

describe('MapEntry', () => {
  it('renames on Enter, refuses a taken key and removes', async () => {
    const { screen, edits } = await renderForm(entry, {
      value: { bindings: { 'cmd+a': 'x', 'cmd+b': 'y' } },
      copy: { 'bindings.*': { label: 'Shortcut', valueLabel: 'Command' } },
    });
    const key = screen.getByRole('textbox', { name: 'Shortcut' });

    await expect.element(screen.getByRole('textbox', { name: 'Command' })).toHaveValue('x');
    await userEvent.fill(key, 'cmd+b');
    await userEvent.keyboard('{Enter}');
    await expect.element(screen.getByRole('alert')).toHaveTextContent('"cmd+b" already exists.');
    await userEvent.fill(key, 'cmd+c');
    await userEvent.keyboard('{Enter}');
    await screen.getByRole('button', { name: 'Remove cmd+a' }).click();
    expect(edits).toEqual([
      { op: 'rename', path: ['bindings'], from: 'cmd+a', to: 'cmd+c' },
      { op: 'remove', path: ['bindings', 'cmd+a'] },
    ]);
  });
});
