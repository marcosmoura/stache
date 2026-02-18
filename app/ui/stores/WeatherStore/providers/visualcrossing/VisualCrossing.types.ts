export interface VisualCrossingResponse {
  queryCost: number;
  latitude: number;
  longitude: number;
  resolvedAddress: string;
  address: string;
  timezone: string;
  tzoffset: number;
  currentConditions: {
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
    solarradiation?: number;
    solarenergy?: number | null;
    conditions: string;
    icon: string;
    moonphase: number;
  };
  days?: Array<{
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
    hours: Array<{
      datetime: string;
      temp: number;
      precip: number;
      precipprob: number;
      preciptype: string[] | null;
      icon: string;
      conditions: string;
    }>;
  }>;
}
