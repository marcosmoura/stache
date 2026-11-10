import { afterEach, describe, expect, test } from 'vitest';
import { page } from 'vitest/browser';

import { barData } from '../../../../../tests/barFixtures';
import { renderWithBarData } from '../../../../../tests/renderBar';

import { Weather } from './Weather';
import { getWeatherIcon, weatherIconMap } from './Weather.icons';
import type { WeatherReading } from './Weather.types';

async function renderWeather(weather: WeatherReading) {
  return renderWithBarData(<Weather />, barData({ weather }));
}

describe('Weather', () => {
  test('renders nothing when the provider is not configured', async () => {
    const { screen } = await renderWeather({ isConfigured: false, isLoading: false });

    expect(screen.container.querySelector('button')).toBeNull();
  });

  test('renders Loading... while loading', async () => {
    const { screen } = await renderWeather({ isConfigured: true, isLoading: true });

    await expect.element(screen.getByText('Loading...')).toBeVisible();
  });

  test('renders Loading... when the fetch failed and no conditions exist', async () => {
    const { screen } = await renderWeather({ isConfigured: true, isLoading: false });

    await expect.element(screen.getByText('Loading...')).toBeVisible();
  });

  test.each([
    [20.2, '21°C'],
    [-5.5, '-5°C'],
    [null, '0°C'],
  ])('renders feels-like %s as %s (rounded up)', async (feelslike, label) => {
    const { screen } = await renderWeather({
      isConfigured: true,
      isLoading: false,
      currentConditions: { feelslike, icon: 'cloudy' },
    });

    await expect.element(screen.getByText(label)).toBeVisible();
  });

  test('uses the icon for the condition and falls back to clear day', () => {
    expect(getWeatherIcon('snow')).toBe(weatherIconMap.snow);
    expect(getWeatherIcon('rainNight')).toBe(weatherIconMap.rainNight);
    expect(getWeatherIcon('nope')).toBe(weatherIconMap.clearDay);
    expect(Object.keys(weatherIconMap)).toHaveLength(16);
  });

  test('caps the label at the legacy width', async () => {
    const { screen } = await renderWeather({
      isConfigured: true,
      isLoading: false,
      currentConditions: { feelslike: 1, icon: 'clearDay' },
    });
    const label = screen.container.querySelector('button > div');

    if (!label) {
      throw new Error('label missing');
    }

    // The test viewport is below 2036px, so the laptop cap applies.
    expect(getComputedStyle(label).maxWidth).toBe('150px');
  });

  describe('on a wide display', () => {
    afterEach(async () => {
      await page.viewport(414, 896);
    });

    test('uses the 230px label cap above 2036px', async () => {
      await page.viewport(2560, 900);

      const { screen } = await renderWeather({
        isConfigured: true,
        isLoading: false,
        currentConditions: { feelslike: 1, icon: 'clearDay' },
      });
      const label = screen.container.querySelector('button > div') as HTMLElement;

      expect(getComputedStyle(label).maxWidth).toBe('230px');
    });
  });
});
