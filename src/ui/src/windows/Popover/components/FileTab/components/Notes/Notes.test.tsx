import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-react';

import { Notes } from './Notes';

describe('Notes', () => {
  it('shows a message as an alert and the conflict after it', async () => {
    const screen = await render(
      <Notes message="Enter a number." conflict={<span>Changed</span>} />,
    );

    await expect.element(screen.getByRole('alert')).toHaveTextContent('Enter a number.');
    await expect.element(screen.getByText('Changed')).toBeVisible();
  });

  it('renders nothing without a message or conflict', async () => {
    const screen = await render(<Notes message={null} />);

    expect(screen.container.textContent).toBe('');
  });
});
