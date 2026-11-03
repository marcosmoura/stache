import type { LocationData } from '../location';

export type WeatherProviderType = 'auto' | 'visual-crossing' | 'open-meteo';

export interface NormalizedCurrentConditions {
  datetime: string;
  temp: number;
  feelslike: number;
  humidity: number;
  dew: number;
  windspeed: number;
  winddir: number;
  windgust: number;
  precip: number;
  precipprob: number;
  preciptype: string[] | null;
  snow: number;
  pressure: number;
  visibility: number;
  cloudcover: number;
  conditions: string;
  icon: string;
  moonphase: number;
  solarradiation?: number;
  solarenergy?: number | null;
}

export interface NormalizedHourlyConditions {
  datetime: string;
  temp: number;
  precip: number;
  precipprob: number;
  preciptype: string[] | null;
  icon: string;
  conditions: string;
}

export interface NormalizedDayData {
  datetime: string;
  temp: number;
  tempmax: number;
  tempmin: number;
  precip: number;
  precipprob: number;
  preciptype: string[] | null;
  snow: number;
  snowdepth: number;
  conditions: string;
  icon: string;
  hours: NormalizedHourlyConditions[];
}

export interface NormalizedWeatherData {
  queryCost?: number;
  latitude: number;
  longitude: number;
  resolvedAddress: string;
  address: string;
  timezone: string;
  tzoffset: number;
  currentConditions: NormalizedCurrentConditions;
  days?: NormalizedDayData[];
}

export interface WeatherProvider {
  name: string;
  type: WeatherProviderType;
  requiresApiKey: boolean;
  fetch(location: LocationData, defaultLocation: string): Promise<NormalizedWeatherData>;
  translateIcon(iconCode: string | number, isDay?: boolean | undefined): string;
}

export interface WeatherConfig {
  provider?: WeatherProviderType;
  visualCrossingApiKey: string;
  defaultLocation: string;
}
