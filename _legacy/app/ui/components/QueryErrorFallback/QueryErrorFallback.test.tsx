import { useState } from 'react';
import { ErrorBoundary } from 'react-error-boundary';

import { describe, expect, test, vi } from 'vitest';
import { render } from 'vitest-browser-react';

import { QueryErrorFallback } from './QueryErrorFallback';

function ThrowingChild({ shouldThrow }: { shouldThrow: boolean }) {
  if (shouldThrow) {
    throw new Error('boom');
  }

  return <div data-testid="recovered">Recovered</div>;
}

describe('QueryErrorFallback', () => {
  test('renders a concise message and retry button when a query fails', async () => {
    const onReset = vi.fn();
    const screen = await render(
      <ErrorBoundary fallback={<QueryErrorFallback />} onReset={onReset}>
        <ThrowingChild shouldThrow />
      </ErrorBoundary>,
    );

    await expect.element(screen.getByText('Something went wrong')).toBeVisible();
    await expect.element(screen.getByRole('button', { name: 'Retry' })).toBeVisible();
  });

  test('resetBoundary re-renders the children so queries can retry', async () => {
    const screen = await render(
      <ErrorBoundary fallback={<QueryErrorFallback />}>
        <ThrowingChild shouldThrow />
      </ErrorBoundary>,
    );

    await expect.element(screen.getByText('Something went wrong')).toBeVisible();

    // Rerender without the failure, then retry — the boundary must reset.
    await screen.rerender(
      <ErrorBoundary fallback={<QueryErrorFallback />}>
        <ThrowingChild shouldThrow={false} />
      </ErrorBoundary>,
    );

    await screen.getByRole('button', { name: 'Retry' }).click();

    await expect.element(screen.getByTestId('recovered')).toBeVisible();
  });

  test('resetBoundary can be invoked from a wrapper component', async () => {
    function BoundaryContainer() {
      const [hasError, setHasError] = useState(true);

      return (
        <ErrorBoundary
          fallback={<QueryErrorFallback />}
          onReset={() => setHasError(false)}
          resetKeys={[hasError]}
        >
          <ThrowingChild shouldThrow={hasError} />
        </ErrorBoundary>
      );
    }

    const screen = await render(<BoundaryContainer />);

    await expect.element(screen.getByText('Something went wrong')).toBeVisible();

    await screen.getByRole('button', { name: 'Retry' }).click();

    await expect.element(screen.getByTestId('recovered')).toBeVisible();
  });
});
