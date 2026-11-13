import { afterEach, describe, expect, it, vi } from 'vitest';

import { renderWithSnapshot } from '../../../../../tests/barSnapshot';
import { barAvailable, barUnavailable } from '../../../../../tests/fixtures';
import { mockHost } from '../../../../../tests/ipc';

import { Cpu } from './Cpu';
import { cpuTemperature, isCpuAlert, selectCpu } from './Cpu.state';

const DANGER = /rgb\(243, 139, 168\)|rgb\(210, 15, 57\)/;

afterEach(() => {
  vi.restoreAllMocks();
});

const iconColor = () => getComputedStyle(document.querySelector('button svg')!).color;

describe('Cpu state', () => {
  it('shows a present, non-zero temperature only', () => {
    expect(cpuTemperature({ usage: 3 })).toBeNull();
    expect(cpuTemperature({ usage: 3, temperature: null })).toBeNull();
    expect(cpuTemperature({ usage: 3, temperature: 0 })).toBeNull();
    expect(cpuTemperature({ usage: 3, temperature: 42 })).toBe(42);
  });

  it('alerts at or above the threshold', () => {
    expect(isCpuAlert({ usage: 1, temperature: 84 }, 85)).toBe(false);
    expect(isCpuAlert({ usage: 1, temperature: 85 }, 85)).toBe(true);
    expect(isCpuAlert({ usage: 1, temperature: 86 }, 85)).toBe(true);
    expect(isCpuAlert({ usage: 1, temperature: 69 }, 70)).toBe(false);
    expect(isCpuAlert({ usage: 1, temperature: 70 }, 70)).toBe(true);
    expect(isCpuAlert({ usage: 1, temperature: null }, 1)).toBe(false);
    expect(isCpuAlert({ usage: 1, temperature: 0 }, 1)).toBe(false);
  });

  it('selects the reading only while available', () => {
    expect(selectCpu(barAvailable('1', { cpu: { usage: 5, temperature: 40 } }))).toEqual({
      usage: 5,
      temperature: 40,
    });
    expect(selectCpu(barAvailable('1'))).toBeNull();
    expect(selectCpu(barUnavailable('1'))).toBeNull();
  });
});

describe('Cpu', () => {
  const options = { temperature_alert: 85 };

  it('renders nothing before the first reading or while unavailable', async () => {
    const { screen, setSnapshot } = await renderWithSnapshot(
      <Cpu id="cpu" zone="end" options={options} />,
    );

    expect(screen.container.childElementCount).toBe(0);
    setSnapshot(barUnavailable('2'));
    await new Promise((done) => setTimeout(done, 20));
    expect(screen.container.childElementCount).toBe(0);
  });

  it('shows usage, then the temperature when present and non-zero', async () => {
    const { screen, setView } = await renderWithSnapshot(
      <Cpu id="cpu" zone="end" options={options} />,
      barAvailable('1', { cpu: { usage: 12, temperature: null } }),
    );
    const button = screen.getByRole('button');

    await expect.element(button).toHaveTextContent('12%');
    setView({ cpu: { usage: 13, temperature: 0 } });
    await expect.element(button).toHaveTextContent('13%');
    setView({ cpu: { usage: 14, temperature: 52 } });
    await expect.element(button).toHaveTextContent('14%52°C');
    expect(button.element().querySelectorAll('span')).toHaveLength(2);
  });

  it('switches to the danger alert at the default threshold (85)', async () => {
    const { screen, setView } = await renderWithSnapshot(
      <Cpu id="cpu" zone="end" options={options} />,
      barAvailable('1', { cpu: { usage: 40, temperature: 84 } }),
    );

    await expect.element(screen.getByRole('button')).toHaveTextContent('40%84°C');
    const calm = button(screen.container).innerHTML;

    expect(iconColor()).not.toMatch(DANGER);
    setView({ cpu: { usage: 40, temperature: 85 } });
    await expect.element(screen.getByRole('button')).toHaveTextContent('40%85°C');
    await vi.waitFor(() => expect(iconColor()).toMatch(DANGER));
    expect(button(screen.container).querySelector('svg')!.innerHTML).not.toBe(calm);
  });

  it('follows a changed temperature_alert arriving in a reload', async () => {
    const { screen, rerender } = await renderWithSnapshot(
      <Cpu id="cpu" zone="end" options={options} />,
      barAvailable('1', { cpu: { usage: 40, temperature: 75 } }),
    );

    await expect.element(screen.getByRole('button')).toHaveTextContent('40%75°C');
    expect(iconColor()).not.toMatch(DANGER);
    await rerender(<Cpu id="cpu" zone="end" options={{ temperature_alert: 75 }} />);
    await vi.waitFor(() => expect(iconColor()).toMatch(DANGER));
    await rerender(<Cpu id="cpu" zone="end" options={{ temperature_alert: 76 }} />);
    await vi.waitFor(() => expect(iconColor()).not.toMatch(DANGER));
  });

  it('sends bar_action("cpu") on click', async () => {
    const host = mockHost().answer('bar_action', { kind: 'completed' });
    const { screen } = await renderWithSnapshot(
      <Cpu id="cpu" zone="end" options={options} />,
      barAvailable('1', { cpu: { usage: 1, temperature: null } }),
    );

    await screen.getByRole('button').click();
    await vi.waitFor(() => expect(host.callsOf('bar_action')).toHaveLength(1));
    expect(host.callsOf('bar_action')[0]!.args).toEqual({ action: 'cpu' });
  });
});

function button(container: HTMLElement) {
  return container.querySelector('button')!;
}
