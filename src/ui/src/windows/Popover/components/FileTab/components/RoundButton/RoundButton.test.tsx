import { Delete02Icon } from '@hugeicons/core-free-icons';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { RoundButton } from './RoundButton';

describe('RoundButton', () => {
  it('is an icon button named by its label, with the label as tooltip', async () => {
    const onClick = vi.fn();
    const screen = await render(
      <RoundButton label="Remove a" icon={Delete02Icon} onClick={onClick} />,
    );
    const button = screen.getByRole('button', { name: 'Remove a' });

    await expect.element(button).toHaveAttribute('title', 'Remove a');
    await button.click();
    expect(onClick).toHaveBeenCalledOnce();
  });

  it('can be disabled', async () => {
    const screen = await render(
      <RoundButton label="Remove a" icon={Delete02Icon} disabled onClick={() => {}} />,
    );

    await expect.element(screen.getByRole('button')).toBeDisabled();
  });
});
