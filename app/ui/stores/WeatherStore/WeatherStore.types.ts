export interface CurrentConditions {
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
  solarradiation: number;
  solarenergy?: number | null;
  conditions: string;
  icon: string;
  moonphase: number;
}

export interface HourlyConditions {
  datetime: string;
  temp: number;
  precip: number;
  precipprob: number;
  preciptype: string[] | null;
  icon: string;
  conditions: string;
}

export interface DayData {
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
  hours: HourlyConditions[];
}

export interface WeatherData {
  queryCost: number;
  latitude: number;
  longitude: number;
  resolvedAddress: string;
  address: string;
  timezone: string;
  tzoffset: number;
  currentConditions: CurrentConditions;
  days?: DayData[];
}

export interface WeatherConfig {
  visualCrossingApiKey: string;
  defaultLocation: string;
}

export interface IpApiResponse {
  city?: string;
  region?: string;
  country_name?: string;
}

export interface IpInfoResponse {
  city?: string;
  region?: string;
  country?: string;
}
