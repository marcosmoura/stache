import type { LocationData } from '../../location';
import type { NormalizedWeatherData, WeatherProvider } from '../types';

import { translateIcon } from './VisualCrossing.icons';
import type { VisualCrossingResponse } from './VisualCrossing.types';

const API_URL = 'https://weather.visualcrossing.com/VisualCrossingWebServices/rest/services';
const API_ELEMENTS =
  'name,address,resolvedAddress,temp,tempmax,tempmin,feelslike,humidity,windspeed,winddir,pressure,visibility,cloudcover,conditions,icon,moonphase,precip,precipprob,preciptype,snow,snowdepth,datetime';
const API_INCLUDE = 'current,hours,days';

export class VisualCrossingProvider implements WeatherProvider {
  readonly name = 'Visual Crossing';
  readonly type = 'visual-crossing' as const;
  readonly requiresApiKey = true;

  private readonly apiKey: string;

  constructor(apiKey: string) {
    this.apiKey = apiKey;
  }

  async fetch(location: LocationData, defaultLocation: string): Promise<NormalizedWeatherData> {
    const displayLocation = location.displayName || defaultLocation;
    const encodedLoc = encodeURIComponent(displayLocation);
    const params = new URLSearchParams({
      key: this.apiKey,
      unitGroup: 'metric',
      elements: API_ELEMENTS,
      include: API_INCLUDE,
      iconSet: 'icons2',
      contentType: 'json',
    });

    const url = `${API_URL}/timeline/${encodedLoc}/next5days?${params.toString()}`;
    const response = await fetch(url);

    if (!response.ok) {
      throw new Error(`Visual Crossing API error: ${response.status}`);
    }

    const data = (await response.json()) as VisualCrossingResponse;

    return this.normalizeResponse(data);
  }

  translateIcon(iconCode: string | number): string {
    return translateIcon(String(iconCode));
  }

  private normalizeResponse(data: VisualCrossingResponse): NormalizedWeatherData {
    const current = data.currentConditions;

    return {
      queryCost: data.queryCost,
      latitude: data.latitude,
      longitude: data.longitude,
      resolvedAddress: data.resolvedAddress,
      address: data.address,
      timezone: data.timezone,
      tzoffset: data.tzoffset,
      currentConditions: {
        datetime: current.datetime,
        temp: current.temp,
        feelslike: current.feelslike,
        humidity: current.humidity,
        dew: current.dew,
        windspeed: current.windspeed,
        winddir: current.winddir,
        windgust: current.windgust,
        precip: current.precip,
        precipprob: current.precipprob,
        preciptype: current.preciptype,
        snow: current.snow,
        pressure: current.pressure,
        visibility: current.visibility,
        cloudcover: current.cloudcover,
        conditions: current.conditions,
        icon: this.translateIcon(current.icon),
        moonphase: current.moonphase,
      },
      days: data.days?.map((day) => ({
        datetime: day.datetime,
        temp: day.temp,
        tempmax: day.tempmax,
        tempmin: day.tempmin,
        precip: day.precip,
        precipprob: day.precipprob,
        preciptype: day.preciptype,
        snow: day.snow,
        snowdepth: day.snowdepth,
        conditions: day.conditions,
        icon: this.translateIcon(day.icon),
        hours: day.hours.map((hour) => ({
          datetime: hour.datetime,
          temp: hour.temp,
          precip: hour.precip,
          precipprob: hour.precipprob,
          preciptype: hour.preciptype,
          icon: this.translateIcon(hour.icon),
          conditions: hour.conditions,
        })),
      })),
    };
  }
}
