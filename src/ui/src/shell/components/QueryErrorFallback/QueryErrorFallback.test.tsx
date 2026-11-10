import { ErrorBoundary } from 'react-error-boundary';

import { describe, expect, test } from 'vitest';
import { render } from 'vitest-browser-react';
import { userEvent } from 'vitest/browser';

import { QueryErrorFallback } from './QueryErrorFallback';

let shouldThrow = true;

function Boom() {
  if (shouldThrow) {
    throw new Error('boom');
  }

  return <p>recovered</p>;
}

describe('QueryErrorFallback', () => {
  test('shows the compact message and retries through the boundary', async () => {
    shouldThrow = true;

    const screen = await render(
      <ErrorBoundary fallback={<QueryErrorFallback />}>
        <Boom />
      </ErrorBoundary>,
    );
    const alert = screen.container.querySelector('[role="alert"]') as HTMLElement;

    await expect.element(screen.getByText('Something went wrong')).toBeVisible();
    expect(getComputedStyle(alert).display).toBe('grid');
    expect(getComputedStyle(alert).paddingLeft).toBe('10px');
    expect(getComputedStyle(alert.querySelector('span') as HTMLElement).color).toBe(
      'rgb(127, 132, 156)',
    );

    shouldThrow = false;
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await expect.element(screen.getByText('recovered')).toBeVisible();
  });
});
