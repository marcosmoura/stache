import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-react';

import { InvalidBanner } from './InvalidBanner';

describe('InvalidBanner', () => {
  it('shows a sentence with the line and hides the raw parser text behind Details', async () => {
    const screen = await render(
      <InvalidBanner
        diagnostic={{ message: 'expected `=` after a key', line: 3, column: 7, field: null }}
      />,
    );

    await expect.element(screen.getByRole('alert')).toBeVisible();
    await expect.element(screen.getByText('This file is invalid and read-only')).toBeVisible();
    await expect
      .element(screen.getByText('Line 3 has a problem: a key is missing its value.'))
      .toBeVisible();
    const raw = screen.getByText('Line 3, column 7: expected `=` after a key');

    await expect.element(raw).not.toBeVisible();
    await screen.getByText('Details').click();
    await expect.element(raw).toBeVisible();
  });

  it('falls back to a generic sentence for an unknown message', async () => {
    const screen = await render(
      <InvalidBanner diagnostic={{ message: 'weird `x`', line: 9, column: 1, field: null }} />,
    );

    await expect.element(screen.getByText("Line 9 can't be read.")).toBeVisible();
  });

  it('has no details without a diagnostic', async () => {
    const screen = await render(<InvalidBanner diagnostic={null} />);

    await expect.element(screen.getByRole('alert')).toBeVisible();
    expect(screen.getByText('Details').query()).toBeNull();
  });
});
