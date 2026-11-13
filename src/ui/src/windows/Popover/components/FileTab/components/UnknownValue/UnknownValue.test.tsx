import { describe, expect, it } from 'vitest';

import { renderForm } from '../../../../../../../tests/settingsForm';

import { UnknownValue } from './UnknownValue';

describe('UnknownValue', () => {
  it('shows the value as text and opens the file in the editor', async () => {
    const { screen, openInEditor } = await renderForm(<UnknownValue value={{ a: 1 }} />);

    await expect.element(screen.getByText('{ a = 1 }')).toBeVisible();
    await screen.getByRole('button', { name: 'Open in editor' }).click();
    expect(openInEditor).toHaveBeenCalledOnce();
  });
});
