import type { BarWeatherLocation } from '@/ipc';

export type Coordinates = { lat: number; lon: number };

/** Where to ask for weather: coordinates (with the text Visual Crossing gets) or a place name. */
export type ResolvedLocation =
  | { kind: 'coordinates'; coordinates: Coordinates; text: string }
  | { kind: 'name'; text: string };

const COORDINATES = /^(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)$/;

/** Parses `lat,lon` decimal degrees in range; anything else is not coordinates. */
export function parseCoordinates(text: string): Coordinates | null {
  const match = COORDINATES.exec(text.trim());

  if (!match) {
    return null;
  }

  const lat = Number(match[1]);
  const lon = Number(match[2]);

  return Math.abs(lat) <= 90 && Math.abs(lon) <= 180 ? { lat, lon } : null;
}

/**
 * The location to request, or `null` when there is none (no request is made):
 * a native fix is used as is; a `lat,lon` default is parsed into numbers and
 * kept as typed for Visual Crossing; any other default is a place name.
 */
export function resolveLocation(location: BarWeatherLocation): ResolvedLocation | null {
  if (location.source === 'coreLocation') {
    const { latitude, longitude } = location;

    if (
      latitude === null ||
      longitude === null ||
      !Number.isFinite(latitude) ||
      !Number.isFinite(longitude)
    ) {
      return null;
    }

    return {
      kind: 'coordinates',
      coordinates: { lat: latitude, lon: longitude },
      text: `${latitude},${longitude}`,
    };
  }

  if (location.source === 'none') {
    return null;
  }

  const text = location.query.trim();

  if (!text) {
    return null;
  }

  const coordinates = parseCoordinates(text);

  return coordinates ? { kind: 'coordinates', coordinates, text } : { kind: 'name', text };
}
