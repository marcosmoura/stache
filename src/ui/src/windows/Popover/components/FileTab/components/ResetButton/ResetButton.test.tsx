import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { ResetButton } from './ResetButton';

describe('ResetButton', () => {
  it('names the setting it resets', async () => {
    const onReset = vi.fn();
    const screen = await render(<ResetButton label="Gaps" disabled={false} onReset={onReset} />);
    const button = screen.getByRole('button', { name: 'Reset Gaps to default' });

    await expect.element(button).toHaveAttribute('title', 'Reset to default');
    await button.click();
    expect(onReset).toHaveBeenCalledOnce();
  });
});
