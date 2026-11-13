import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { Stepper } from './Stepper';

describe('Stepper', () => {
  it('steps up and down and stops at its bounds', async () => {
    const onStep = vi.fn();
    const screen = await render(
      <Stepper
        label="Ratio"
        value={10}
        minimum={0}
        maximum={10}
        disabled={false}
        onStep={onStep}
      />,
    );

    await expect.element(screen.getByRole('button', { name: 'Increase Ratio' })).toBeDisabled();
    await screen.getByRole('button', { name: 'Decrease Ratio' }).click();
    expect(onStep).toHaveBeenCalledWith(-1);
  });
});
