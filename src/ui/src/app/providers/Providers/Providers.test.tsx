import { useQueryClient } from '@tanstack/react-query';
import * as m from 'motion/react-m';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-react';

import { Providers } from './Providers';

describe('Providers', () => {
  it('renders its children', async () => {
    const screen = await render(
      <Providers>
        <p>content</p>
      </Providers>,
    );

    await expect.element(screen.getByText('content')).toBeVisible();
  });

  it('supplies a query client and the motion configuration', async () => {
    function Probe() {
      const client = useQueryClient();

      return <m.div data-testid="probe" data-mounted={client.isMutating() === 0} />;
    }

    const screen = await render(
      <Providers>
        <Probe />
      </Providers>,
    );

    await expect.element(screen.getByTestId('probe')).toHaveAttribute('data-mounted', 'true');
  });
});
