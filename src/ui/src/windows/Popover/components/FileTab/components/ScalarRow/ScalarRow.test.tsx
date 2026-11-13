import { describe, expect, it } from 'vitest';
import { userEvent } from 'vitest/browser';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { ScalarRow } from './ScalarRow';

describe('ScalarRow', () => {
  it('a switch shows the default and writes `set` at once', async () => {
    const { screen, edits } = await renderForm(
      <ScalarRow
        node={node.boolean()}
        path={['enabled']}
        at="enabled"
        label="Enabled"
        inTable
        depth={1}
      />,
      { defaults: { enabled: false } },
    );

    await expect.element(screen.getByText('default')).toBeVisible();
    await screen.getByRole('switch', { name: 'Enabled' }).click();
    expect(edits).toEqual([{ op: 'set', path: ['enabled'], value: true }]);
  });

  it('a number shows its unit, refuses a bad value locally and saves on Enter', async () => {
    const { screen, edits } = await renderForm(
      <ScalarRow
        node={node.number(0, 100)}
        path={['ratio']}
        at="ratio"
        label="Ratio"
        inTable
        depth={1}
      />,
      { value: { ratio: 60 }, copy: { ratio: { label: 'Ratio', unit: '%' } } },
    );
    const field = screen.getByRole('textbox', { name: 'Ratio' });

    await expect.element(screen.getByText('%')).toBeVisible();
    await userEvent.fill(field, '200');
    await userEvent.keyboard('{Enter}');
    await expect.element(screen.getByRole('alert')).toHaveTextContent('Must be at most 100.');
    expect(edits).toEqual([]);
    await userEvent.fill(field, '70');
    await userEvent.keyboard('{Enter}');
    await expect.poll(() => edits).toEqual([{ op: 'set', path: ['ratio'], value: 70 }]);
  });

  it('a set key has Reset to default; a constant renders nothing', async () => {
    const { screen, edits } = await renderForm(
      <>
        <ScalarRow node={node.string()} path={['name']} at="name" label="Name" inTable depth={1} />
        <ScalarRow
          node={node.const('clock')}
          path={['widget']}
          at="widget"
          label="Widget"
          inTable
          depth={1}
        />
      </>,
      { value: { name: 'a', widget: 'clock' } },
    );

    expect(screen.getByText('Widget').query()).toBeNull();
    await screen.getByRole('button', { name: 'Reset Name to default' }).click();
    expect(edits).toEqual([{ op: 'remove', path: ['name'] }]);
  });
});
