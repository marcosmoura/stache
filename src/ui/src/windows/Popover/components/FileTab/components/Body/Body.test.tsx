import { describe, expect, it } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { Body } from './Body';

describe('Body', () => {
  it('renders a table, a map and a list by kind', async () => {
    const { screen } = await renderForm(
      <>
        <Body
          node={node.object({ on: node.boolean() })}
          path={['t']}
          at="t"
          label="T"
          inTable={false}
          depth={1}
        />
        <Body
          node={node.map(node.string())}
          path={['m']}
          at="m"
          label="M"
          inTable={false}
          depth={1}
        />
        <Body
          node={node.array(node.string())}
          path={['l']}
          at="l"
          label="L"
          inTable={false}
          depth={1}
        />
      </>,
      {
        value: { t: { on: true }, m: { k: 'v' }, l: ['x'] },
        copy: { m: { label: 'M', noun: 'Entry' }, l: { label: 'L', noun: 'Item' } },
      },
    );

    await expect.element(screen.getByRole('switch', { name: 'On' })).toBeChecked();
    await expect.element(screen.getByRole('group', { name: 'k' })).toBeVisible();
    await expect.element(screen.getByRole('textbox', { name: 'Item 1' })).toHaveValue('x');
  });
});
