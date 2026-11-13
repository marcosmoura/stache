import { describe, expect, it, vi } from 'vitest';
import { userEvent } from 'vitest/browser';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { NewMapEntry } from './NewMapEntry';

const map = node.map(node.string());

function row(onClose: () => void) {
  return (
    <ul>
      <NewMapEntry
        node={map}
        path={['bindings']}
        keys={['cmd+a']}
        noun="Shortcut"
        onClose={onClose}
      />
    </ul>
  );
}

describe('NewMapEntry', () => {
  it('starts empty and focused, and Esc closes without writing', async () => {
    const onClose = vi.fn();
    const { screen, edits } = await renderForm(row(onClose));
    const key = screen.getByRole('textbox', { name: 'Shortcut' });

    await expect.element(key).toHaveValue('');
    await expect.element(key).toHaveFocus();
    await expect.element(key).toHaveAttribute('placeholder', 'New shortcut');
    await userEvent.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledOnce();
    expect(edits).toEqual([]);
  });

  it('asks for a key, refuses a taken one, then writes once on Enter', async () => {
    const onClose = vi.fn();
    const { screen, edits } = await renderForm(row(onClose));

    await userEvent.keyboard('{Enter}');
    await expect.element(screen.getByRole('alert')).toHaveTextContent('Enter a key.');
    await userEvent.keyboard('cmd+a{Enter}');
    await expect.element(screen.getByRole('alert')).toHaveTextContent('"cmd+a" already exists.');
    await userEvent.fill(screen.getByRole('textbox'), ' cmd+k ');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => onClose.mock.calls.length).toBe(1);
    expect(edits).toEqual([{ op: 'set', path: ['bindings', 'cmd+k'], value: '' }]);
  });

  it('closes on a blur with nothing typed, and keeps the row when the write fails', async () => {
    const closeEmpty = vi.fn();
    const empty = await renderForm(row(closeEmpty));

    (document.activeElement as HTMLElement).blur();
    expect(closeEmpty).toHaveBeenCalledOnce();
    expect(empty.edits).toEqual([]);
    empty.screen.unmount();

    const closeFailed = vi.fn();
    const failed = await renderForm(row(closeFailed), {
      result: { status: 'error', message: 'Refused.' },
    });

    await userEvent.keyboard('cmd+k');
    (document.activeElement as HTMLElement).blur();
    await expect.element(failed.screen.getByRole('alert')).toHaveTextContent('Refused.');
    expect(closeFailed).not.toHaveBeenCalled();
    expect(failed.edits).toHaveLength(1);
  });
});
