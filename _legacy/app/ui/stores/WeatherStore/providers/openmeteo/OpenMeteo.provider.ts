import type { LocationData } from '../../location';
import type { NormalizedWeatherData, WeatherProvider } from '../types';

import { translateIcon, getWeatherCondition, getPrecipType } from './OpenMeteo.icons';
import type { OpenMeteoResponse } from './OpenMeteo.types';

const API_URL = 'https://api.open-meteo.com/v1/forecast';

export class OpenMeteoProvider implements WeatherProvider {
  readonly name = 'Open Meteo';
  readonly type = 'open-meteo' as const;
  readonly requiresApiKey = false;

  async fetch(location: LocationData, defaultLocation: string): Promise<NormalizedWeatherData> {
    let lat: number;
    let lon: number;
    let displayName: string;

    if (location.coordinates) {
      lat = location.coordinates.lat;
      lon = location.coordinates.lon;
      displayName = location.displayName || defaultLocation;
    } else {
      const geocodeData = await this.geocodeLocation(location.displayName || defaultLocation);
      if (geocodeData) {
        lat = geocodeData.lat;
        lon = geocodeData.lon;
        displayName = geocodeData.name;
      } else {
        throw new Error('Failed to geocode location');
      }
    }

    const params = new URLSearchParams({
      latitude: lat.toString(),
      longitude: lon.toString(),
      current: [
        'temperature_2m',
        'relative_humidity_2m',
        'apparent_temperature',
        'is_day',
        'precipitation',
        'weather_code',
        'wind_speed_10m',
        'wind_direction_10m',
        'wind_gusts_10m',
      ].join(','),
      hourly: [
        'temperature_2m',
        'relative_humidity_2m',
        'precipitation_probability',
        'precipitation',
        'weather_code',
        'cloud_cover',
        'wind_speed_10m',
        'wind_direction_10m',
        'wind_gusts_10m',
        'visibility',
        'pressure_msl',
      ].join(','),
      daily: [
        'weather_code',
        'temperature_2m_max',
        'temperature_2m_min',
        'precipitation_sum',
        'precipitation_probability_max',
        'wind_speed_10m_max',
        'sunrise',
        'sunset',
      ].join(','),
      timezone: 'auto',
      forecast_days: '5',
    });

    const url = `${API_URL}?${params.toString()}`;
    const response = await fetch(url);

    if (!response.ok) {
      throw new Error(`Open Meteo API error: ${response.status}`);
    }

    const data = (await response.json()) as OpenMeteoResponse;

    return this.normalizeResponse(data, displayName);
  }

  translateIcon(iconCode: string | number, isDay?: boolean): string {
    return translateIcon(Number(iconCode), isDay ?? true);
  }

  private async geocodeLocation(
    locationName: string,
  ): Promise<{ lat: number; lon: number; name: string } | null> {
    const params = new URLSearchParams({
      name: locationName,
      count: '1',
      language: 'en',
      format: 'json',
    });

    try {
      const response = await fetch(`https://geocoding-api.open-meteo.com/v1/search?${params}`);

      if (!response.ok) {
        return null;
      }

      const result = (await response.json()) as {
        results?: Array<{
          latitude: number;
          longitude: number;
          name: string;
          country?: string;
        }>;
      };

      if (result.results && result.results.length > 0) {
        const r = result.results[0];
        return {
          lat: r.latitude,
          lon: r.longitude,
          name: r.country ? `${r.name}, ${r.country}` : r.name,
        };
      }

      return null;
    } catch {
      return null;
    }
  }

  private normalizeResponse(data: OpenMeteoResponse, displayName: string): NormalizedWeatherData {
    const current = data.current;
    const weatherCode = current.weather_code;

    return {
      latitude: data.latitude,
      longitude: data.longitude,
      resolvedAddress: displayName,
      address: displayName.split(',')[0]?.trim() || displayName,
      timezone: data.timezone,
      tzoffset: data.utc_offset_seconds / 3600,
      currentConditions: {
        datetime: current.time,
        temp: current.temperature_2m,
        feelslike: current.apparent_temperature,
        humidity: current.relative_humidity_2m,
        dew: 0,
        windspeed: current.wind_speed_10m,
        winddir: current.wind_direction_10m,
        windgust: current.wind_gusts_10m,
        precip: current.precipitation,
        precipprob: 0,
        preciptype: getPrecipType(weatherCode),
        snow: 0,
        pressure: data.hourly.pressure_msl[0] ?? 0,
        visibility: data.hourly.visibility[0] ?? 10000,
        cloudcover: data.hourly.cloud_cover[0] ?? 0,
        conditions: getWeatherCondition(weatherCode),
        icon: this.translateIcon(weatherCode, current.is_day === 1),
        moonphase: 0,
      },
      days: data.daily.time.map((dayTime, index) => {
        const dailyWeatherCode = data.daily.weather_code[index];
        return {
          datetime: dayTime,
          temp: (data.daily.temperature_2m_max[index] + data.daily.temperature_2m_min[index]) / 2,
          tempmax: data.daily.temperature_2m_max[index],
          tempmin: data.daily.temperature_2m_min[index],
          precip: data.daily.precipitation_sum[index] ?? 0,
          precipprob: data.daily.precipitation_probability_max[index] ?? 0,
          preciptype: getPrecipType(dailyWeatherCode),
          snow: 0,
          snowdepth: 0,
          conditions: getWeatherCondition(dailyWeatherCode),
          icon: this.translateIcon(dailyWeatherCode, true),
          hours: this.getDayHours(data, index),
        };
      }),
    };
  }

  private getDayHours(
    data: OpenMeteoResponse,
    dayIndex: number,
  ): Array<{
    datetime: string;
    temp: number;
    precip: number;
    precipprob: number;
    preciptype: string[] | null;
    icon: string;
    conditions: string;
  }> {
    const dayStartIndex = dayIndex * 24;
    const hours: Array<{
      datetime: string;
      temp: number;
      precip: number;
      precipprob: number;
      preciptype: string[] | null;
      icon: string;
      conditions: string;
    }> = [];

    for (let i = 0; i < 24; i++) {
      const hourIndex = dayStartIndex + i;
      if (hourIndex >= data.hourly.time.length) break;

      const weatherCode = data.hourly.weather_code[hourIndex];
      hours.push({
        datetime: data.hourly.time[hourIndex],
        temp: data.hourly.temperature_2m[hourIndex] ?? 0,
        precip: data.hourly.precipitation[hourIndex] ?? 0,
        precipprob: data.hourly.precipitation_probability[hourIndex] ?? 0,
        preciptype: getPrecipType(weatherCode),
        icon: this.translateIcon(weatherCode, true),
        conditions: getWeatherCondition(weatherCode),
      });
    }

    return hours;
  }
}
