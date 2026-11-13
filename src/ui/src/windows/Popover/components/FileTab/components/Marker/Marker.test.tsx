import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-react';

import { Marker } from './Marker';

describe('Marker', () => {
  it('shows its note', async () => {
    const screen = await render(<Marker>default</Marker>);

    await expect.element(screen.getByText('default')).toBeVisible();
  });
});
