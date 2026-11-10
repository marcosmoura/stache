import { describe, expect, test } from 'vitest';
import { render } from 'vitest-browser-react';

import { useMediaQuery } from './useMediaQuery';

function Probe({ query }: { query: string }) {
  return <p>{String(useMediaQuery(query))}</p>;
}

describe('useMediaQuery', () => {
  test('reports whether the query matches', async () => {
    const yes = await render(<Probe query="(min-width: 1px)" />);

    expect(yes.container.textContent).toBe('true');
    await yes.unmount();

    const no = await render(<Probe query="(min-width: 99999px)" />);

    expect(no.container.textContent).toBe('false');
  });
});
