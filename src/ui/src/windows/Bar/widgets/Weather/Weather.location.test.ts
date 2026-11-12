import { describe, expect, it } from 'vitest';

import { parseCoordinates, resolveLocation } from './Weather.location';

describe('weather location', () => {
  it('parses lat,lon decimal degrees in range only', () => {
    expect(parseCoordinates('52.52,13.405')).toEqual({ lat: 52.52, lon: 13.405 });
    expect(parseCoordinates(' -33.9 , 151 ')).toEqual({ lat: -33.9, lon: 151 });
    expect(parseCoordinates('90,180')).toEqual({ lat: 90, lon: 180 });
    expect(parseCoordinates('91,0')).toBeNull();
    expect(parseCoordinates('0,181')).toBeNull();
    expect(parseCoordinates('Berlin')).toBeNull();
    expect(parseCoordinates('1,2,3')).toBeNull();
    expect(parseCoordinates('')).toBeNull();
  });

  it('uses a native fix as is', () => {
    expect(
      resolveLocation({
        source: 'coreLocation',
        latitude: 48.85,
        longitude: 2.35,
        reducedAccuracy: true,
      }),
    ).toEqual({ kind: 'coordinates', coordinates: { lat: 48.85, lon: 2.35 }, text: '48.85,2.35' });
  });

  it('has no location for a fix without coordinates', () => {
    for (const [latitude, longitude] of [
      [null, 1],
      [1, null],
      [Number.NaN, 1],
      [1, Number.POSITIVE_INFINITY],
    ] as const) {
      expect(
        resolveLocation({ source: 'coreLocation', latitude, longitude, reducedAccuracy: false }),
      ).toBeNull();
    }
  });

  it('parses a lat,lon default and keeps the text as typed', () => {
    expect(resolveLocation({ source: 'defaultLocation', query: ' 52.52, 13.405 ' })).toEqual({
      kind: 'coordinates',
      coordinates: { lat: 52.52, lon: 13.405 },
      text: '52.52, 13.405',
    });
  });

  it('treats any other default as a place name', () => {
    expect(resolveLocation({ source: 'defaultLocation', query: 'São Paulo, BR' })).toEqual({
      kind: 'name',
      text: 'São Paulo, BR',
    });
  });

  it('has no location for none or an empty default', () => {
    expect(resolveLocation({ source: 'none' })).toBeNull();
    expect(resolveLocation({ source: 'defaultLocation', query: '  ' })).toBeNull();
  });
});
