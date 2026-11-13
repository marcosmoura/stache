import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-react';

import { Row } from './Row';

describe('Row', () => {
  it('labels its control, shows the hint and marker, and carries its path', async () => {
    const screen = await render(
      <Row
        id="field"
        path={['workspaces', 1, 'layout']}
        label="Layout"
        hint="How windows are arranged."
        helpId="field-help"
        pending
        marker="default"
        controls={<input id="field" aria-describedby="field-help" />}
      />,
    );

    await expect.element(screen.getByRole('textbox', { name: 'Layout' })).toBeVisible();
    await expect
      .element(screen.getByRole('textbox'))
      .toHaveAccessibleDescription('How windows are arranged.');
    await expect.element(screen.getByText('default')).toBeVisible();
    const row = screen.container.querySelector('[data-path="workspaces[1].layout"]');

    expect(row).toHaveAttribute('aria-busy', 'true');
  });
});
