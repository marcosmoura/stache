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

export interface WeatherData {
  queryCost: number;
  latitude: number;
  longitude: number;
  resolvedAddress: string;
  address: string;
  timezone: string;
  tzoffset: number;
  currentConditions: CurrentConditions;
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
