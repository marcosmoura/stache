import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-react';

import { OVERVIEW_TAB, popoverStore } from './popover';

function Probe() {
  const tab = popoverStore.useStore((state) => state.tab);
  const { select } = popoverStore.useStore((state) => state.actions);

  return (
    <button type="button" onClick={() => select('bar.toml')}>
      {tab}
    </button>
  );
}

describe('popoverStore', () => {
  it('starts on Overview and keeps the selected tab', async () => {
    const screen = await render(
      <popoverStore.Provider>
        <Probe />
      </popoverStore.Provider>,
    );

    await expect.element(screen.getByRole('button', { name: OVERVIEW_TAB })).toBeVisible();
    await screen.getByRole('button').click();
    await expect.element(screen.getByRole('button', { name: 'bar.toml' })).toBeVisible();
  });
});
