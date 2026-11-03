export interface LocationCoordinates {
  lat: number;
  lon: number;
}

export interface LocationData {
  coordinates?: LocationCoordinates;
  displayName: string;
}

export interface IpApiResponse {
  city?: string;
  region?: string;
  country_name?: string;
  latitude?: number;
  longitude?: number;
}

export interface IpInfoResponse {
  city?: string;
  region?: string;
  country?: string;
  loc?: string;
}
