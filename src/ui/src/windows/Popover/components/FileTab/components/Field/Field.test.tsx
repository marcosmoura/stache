import { describe, expect, it } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { Field } from './Field';

describe('Field', () => {
  it('renders the component of its kind, and nothing for a constant', async () => {
    const { screen } = await renderForm(
      <>
        <Field node={node.boolean()} path={['on']} at="on" label="On" inTable depth={1} />
        <Field node={node.const('x')} path={['tag']} at="tag" label="Tag" inTable depth={1} />
        <Field node={node.unknown()} path={['odd']} at="odd" label="Odd" inTable depth={1} />
      </>,
      { value: { odd: 1 } },
    );

    await expect.element(screen.getByRole('switch', { name: 'On' })).toBeVisible();
    expect(screen.getByText('Tag').query()).toBeNull();
    await expect.element(screen.getByRole('button', { name: 'Open in editor' })).toBeVisible();
  });
});
