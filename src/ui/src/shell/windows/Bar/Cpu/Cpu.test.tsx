import { describe, expect, test } from 'vitest';

import { barData } from '../../../../../tests/barFixtures';
import { renderWithBarData } from '../../../../../tests/renderBar';

import { Cpu } from './Cpu';

describe('Cpu', () => {
  test('renders nothing while the reading is unresolved', async () => {
    const { screen } = await renderWithBarData(<Cpu />, barData({ cpu: undefined }));

    expect(screen.container.querySelector('button')).toBeNull();
  });

  test('renders the usage without a temperature', async () => {
    const { screen } = await renderWithBarData(
      <Cpu />,
      barData({ cpu: { usage: 30, temperature: null } }),
    );

    await expect.element(screen.getByText('30%')).toBeVisible();
    expect(screen.container.querySelectorAll('button span')).toHaveLength(1);
    expect(screen.container.querySelector('svg')).not.toBeNull();
  });

  test('renders the legacy no-data branch for a null reading', async () => {
    const { screen } = await renderWithBarData(<Cpu />, barData({ cpu: null }));

    await expect.element(screen.getByText('0%')).toBeVisible();
  });

  test('renders usage and temperature below the threshold in the text color', async () => {
    const { screen } = await renderWithBarData(
      <Cpu />,
      barData({ cpu: { usage: 45, temperature: 84 } }),
    );

    await expect.element(screen.getByText('84°C')).toBeVisible();
    expect(screen.container.querySelector('svg')?.getAttribute('color')).toBe('#cdd6f4');
  });

  test('switches icon and color at 85 degrees', async () => {
    const cool = await renderWithBarData(<Cpu />, barData({ cpu: { usage: 1, temperature: 84 } }));
    const coolPaths = cool.screen.container.querySelector('svg')?.innerHTML;

    await cool.screen.unmount();

    const hot = await renderWithBarData(<Cpu />, barData({ cpu: { usage: 95, temperature: 85 } }));
    const svg = hot.screen.container.querySelector('svg');

    await expect.element(hot.screen.getByText('85°C')).toBeVisible();
    expect(svg?.getAttribute('color')).toBe('#f38ba8');
    expect(svg?.innerHTML).not.toBe(coolPaths);
  });

  test('is not interactive beyond the legacy hover styling', async () => {
    const { screen } = await renderWithBarData(
      <Cpu />,
      barData({ cpu: { usage: 1, temperature: null } }),
    );

    expect(screen.container.querySelector('button')?.type).toBe('button');
  });
});
