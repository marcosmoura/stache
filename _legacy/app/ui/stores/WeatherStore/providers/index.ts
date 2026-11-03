import { OpenMeteoProvider } from './openmeteo';
import type { WeatherProvider, WeatherConfig } from './types';
import { VisualCrossingProvider } from './visualcrossing';

export * from './types';

export const createWeatherProvider = (config: WeatherConfig): WeatherProvider => {
  if (config.provider === 'open-meteo') {
    return new OpenMeteoProvider();
  }

  if (config.provider === 'visual-crossing') {
    if (!config.visualCrossingApiKey) {
      throw new Error('Visual Crossing API key is required');
    }
    return new VisualCrossingProvider(config.visualCrossingApiKey);
  }

  if (config.provider === 'auto' || config.provider === undefined) {
    if (config.visualCrossingApiKey) {
      return new VisualCrossingProvider(config.visualCrossingApiKey);
    }

    return new OpenMeteoProvider();
  }

  return new OpenMeteoProvider();
};

export const isProviderAvailable = (config: WeatherConfig): boolean => {
  if (config.provider === 'open-meteo') {
    return true;
  }

  if (config.provider === 'visual-crossing' || config.provider === 'auto') {
    if (config.visualCrossingApiKey) {
      return true;
    }
  }

  if (config.provider === 'auto' || config.provider === undefined) {
    return true;
  }

  return false;
};
