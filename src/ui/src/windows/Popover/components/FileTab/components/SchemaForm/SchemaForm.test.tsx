import { describe, expect, it } from 'vitest';

import { node, renderForm } from '../../../../../../../tests/settingsForm';

import { SchemaForm } from './SchemaForm';

describe('SchemaForm', () => {
  it('renders a file from its tree with the labels and hints of its copy', async () => {
    const tree = node.object({ enabled: node.boolean() });
    const { screen } = await renderForm(
      <SchemaForm
        tree={tree}
        copy={{ enabled: { label: 'Turn on', hint: 'Starts the module.' } }}
      />,
    );

    await expect.element(screen.getByTestId('settings-form')).toBeVisible();
    await expect
      .element(screen.getByRole('switch', { name: 'Turn on' }))
      .toHaveAccessibleDescription('Starts the module.');
  });

  it('renders a non-table root as one field', async () => {
    const { screen } = await renderForm(
      <SchemaForm tree={node.boolean()} copy={{ '': { label: 'Whole file' } }} />,
    );

    await expect.element(screen.getByRole('switch', { name: 'Whole file' })).toBeVisible();
  });
});
