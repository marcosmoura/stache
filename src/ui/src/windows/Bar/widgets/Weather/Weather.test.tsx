import { CloudAngledRainIcon, MoonIcon, SunIcon } from '@hugeicons/core-free-icons';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { BarWeatherOutcome_Serialize, BarWeather_Serialize } from '@/ipc';

import { renderWithSnapshot } from '../../../../../tests/barSnapshot';
import { stubRemoteFetch } from '../../../../../tests/fetch';
import { barAvailable, barUnavailable } from '../../../../../tests/fixtures';
import { mockHost, type MockHost } from '../../../../../tests/ipc';

import { Weather } from './Weather';
import { temperatureLabel, weatherLook } from './Weather.state';

const KEY = 'SENTINEL-KEY-0123';

const ready = (overrides: Partial<BarWeather_Serialize> = {}): BarWeatherOutcome_Serialize => ({
  kind: 'ready',
  weather: {
    configRevision: '1',
    provider: 'auto',
    apiKey: KEY,
    location: { source: 'defaultLocation', query: '41.15,-8.61' },
    refreshIntervalMinutes: 20,
    ...overrides,
  },
});

const vc = (feelslike: number | null, icon = 'rain') =>
  Promise.resolve(new Response(JSON.stringify({ currentConditions: { feelslike, icon } })));

let host: MockHost;
let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  host = mockHost().answer('bar_action', { kind: 'completed' });
  fetchMock = vi.fn(() => vc(12.1));
  stubRemoteFetch(fetchMock);
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const widget = <Weather id="weather" zone="end" options={{}} />;

describe('Weather state', () => {
  it('rounds the feels-like temperature up, and shows 0°C without one', () => {
    expect(temperatureLabel({ feelslike: 12.1, condition: 'rain' })).toBe('13°C');
    expect(temperatureLabel({ feelslike: -0.5, condition: 'rain' })).toBe('0°C');
    expect(temperatureLabel({ feelslike: -3.7, condition: 'rain' })).toBe('-3°C');
    expect(temperatureLabel({ feelslike: null, condition: 'rain' })).toBe('0°C');
  });

  it('picks the look for each state', () => {
    const configured = { configured: true, refreshIntervalMinutes: 20 };

    expect(weatherLook(undefined, undefined)).toBeNull();
    expect(weatherLook({ kind: 'unconfigured' }, configured)).toBeNull();
    expect(weatherLook(undefined, { configured: false, refreshIntervalMinutes: null })).toBeNull();
    expect(weatherLook(undefined, configured)).toEqual({ icon: SunIcon, label: 'Loading...' });
    expect(
      weatherLook(
        {
          kind: 'ready',
          conditions: { feelslike: 1, condition: 'clearNight' },
          refreshIntervalMinutes: 20,
        },
        configured,
      ),
    ).toEqual({ icon: MoonIcon, label: '1°C' });
    expect(
      weatherLook(
        {
          kind: 'ready',
          conditions: { feelslike: 1, condition: 'rain' },
          refreshIntervalMinutes: 20,
        },
        undefined,
      )?.icon,
    ).toBe(CloudAngledRainIcon);
  });
});

describe('Weather', () => {
  it('renders the icon and the rounded-up temperature', async () => {
    host.answer('bar_weather', ready());

    const { screen } = await renderWithSnapshot(widget, barAvailable('1'));
    const button = screen.getByRole('button');

    await expect.element(button).toHaveTextContent('13°C');
    expect(button.element().querySelector('svg')).not.toBeNull();
    expect(document.body.innerHTML).not.toContain(KEY);
    expect(document.body.innerHTML).not.toContain('41.15');
  });

  it('renders 0°C without a feels-like value', async () => {
    host.answer('bar_weather', ready());
    fetchMock.mockImplementation(() => vc(null));

    const { screen } = await renderWithSnapshot(widget, barAvailable('1'));

    await expect.element(screen.getByRole('button')).toHaveTextContent('0°C');
  });

  it.each([
    ['a non-OK status', () => Promise.resolve(new Response(KEY, { status: 500 }))],
    ['malformed JSON', () => Promise.resolve(new Response('<html>'))],
    ['a wrong shape', () => Promise.resolve(new Response('{"currentConditions":{}}'))],
    ['a network error', () => Promise.reject(new TypeError(`failed ${KEY}`))],
  ])('renders Loading... after %s, with no secret anywhere', async (_name, answer) => {
    const debug = vi.spyOn(console, 'debug');
    const error = vi.spyOn(console, 'error');
    const warn = vi.spyOn(console, 'warn');

    host.answer('bar_weather', ready());
    fetchMock.mockImplementation(answer);

    const { screen } = await renderWithSnapshot(widget, barAvailable('1'));

    await expect.element(screen.getByRole('button')).toHaveTextContent('Loading...');
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalled());

    const logged = JSON.stringify([debug.mock.calls, error.mock.calls, warn.mock.calls]);

    expect(logged).not.toContain(KEY);
    expect(logged).not.toContain('41.15');
    expect(document.body.innerHTML).not.toContain(KEY);
  });

  it('renders Loading... without a location', async () => {
    host.answer('bar_weather', ready({ location: { source: 'none' } }));

    const { screen } = await renderWithSnapshot(widget, barAvailable('1'));

    await expect.element(screen.getByRole('button')).toHaveTextContent('Loading...');
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('renders Loading... while the host answers unavailable', async () => {
    host.answer('bar_weather', { kind: 'unavailable', reason: 'stopped', detail: null });

    const { screen } = await renderWithSnapshot(widget, barAvailable('1'));

    await expect.element(screen.getByRole('button')).toHaveTextContent('Loading...');
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('is hidden while unconfigured', async () => {
    host.answer('bar_weather', ready({ provider: 'visualCrossing', apiKey: '' }));

    const { screen } = await renderWithSnapshot(widget, barAvailable('1'));

    await vi.waitFor(() => expect(host.callsOf('bar_weather')).toHaveLength(1));
    await new Promise((done) => setTimeout(done, 30));
    expect(screen.container.childElementCount).toBe(0);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('is hidden and calls nothing while the Bar is not available', async () => {
    host.answer('bar_weather', ready());

    const { screen } = await renderWithSnapshot(widget, barUnavailable('1'));

    await new Promise((done) => setTimeout(done, 30));
    expect(screen.container.childElementCount).toBe(0);
    expect(host.callsOf('bar_weather')).toHaveLength(0);
  });

  it('refetches for a new epoch and keeps the reading meanwhile', async () => {
    host.answer('bar_weather', ready());

    const { screen, setView } = await renderWithSnapshot(widget, barAvailable('1'));
    const button = screen.getByRole('button');

    await expect.element(button).toHaveTextContent('13°C');
    fetchMock.mockImplementation(() => vc(20.2, 'clear-night'));
    setView({ weatherEpoch: '1' });
    await expect.element(button).toHaveTextContent('21°C');
    expect(host.callsOf('bar_weather')).toHaveLength(2);
  });

  it('sends bar_action("weather") on click', async () => {
    host.answer('bar_weather', ready());

    const { screen } = await renderWithSnapshot(widget, barAvailable('1'));

    await expect.element(screen.getByRole('button')).toHaveTextContent('13°C');
    await screen.getByRole('button').click();
    await vi.waitFor(() => expect(host.callsOf('bar_action')).toHaveLength(1));
    expect(host.callsOf('bar_action')[0]!.args).toEqual({ action: 'weather' });
  });
});
