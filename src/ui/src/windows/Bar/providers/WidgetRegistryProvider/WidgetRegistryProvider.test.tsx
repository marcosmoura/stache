import { describe, expect, it } from 'vitest';

import { fixtureRegistry } from '../../../../../tests/fixtureRegistry';
import { render } from '../../../../../tests/render';
import { productionRegistry } from '../../widgets/registry';
import { useWidgetRegistry } from '../../widgets/resolve';

import { WidgetRegistryProvider } from './WidgetRegistryProvider';

function Probe() {
  const registry = useWidgetRegistry();

  return (
    <p data-testid="probe">
      {registry === fixtureRegistry
        ? 'fixture'
        : registry === productionRegistry
          ? 'production'
          : '?'}
    </p>
  );
}

describe('WidgetRegistryProvider', () => {
  it('defaults to the production registry', async () => {
    const screen = await render(<Probe />);

    await expect.element(screen.getByTestId('probe')).toHaveTextContent('production');
  });

  it('supplies another registry to its subtree', async () => {
    const screen = await render(
      <WidgetRegistryProvider registry={fixtureRegistry}>
        <Probe />
      </WidgetRegistryProvider>,
    );

    await expect.element(screen.getByTestId('probe')).toHaveTextContent('fixture');
  });
});
