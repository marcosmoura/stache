/** Provider vocabularies mapped to the keys of `weatherIconMap`, ported from legacy. */

const openMeteoIconMap: Record<number, string> = {
  0: 'clearDay',
  1: 'clearDay',
  2: 'partlyCloudyDay',
  3: 'cloudy',
  45: 'fog',
  48: 'fog',
  51: 'rain',
  53: 'rain',
  55: 'rain',
  56: 'rain',
  57: 'rain',
  61: 'rain',
  63: 'rain',
  65: 'rain',
  66: 'rain',
  67: 'rain',
  71: 'snow',
  73: 'snow',
  75: 'snow',
  77: 'snow',
  80: 'rainDay',
  81: 'rainDay',
  82: 'rainDay',
  85: 'snowShowersDay',
  86: 'snowShowersDay',
  95: 'thunder',
  96: 'thunder',
  99: 'thunder',
};

/** Open Meteo WMO weather code to an icon key; unknown codes read as clear day. */
export function translateOpenMeteoIcon(weatherCode: number, isDay = true): string {
  const icon = openMeteoIconMap[weatherCode] ?? 'clearDay';

  if (!isDay) {
    switch (icon) {
      case 'clearDay':
        return 'clearNight';
      case 'partlyCloudyDay':
        return 'partlyCloudyNight';
      case 'rainDay':
        return 'rainNight';
      case 'snowShowersDay':
        return 'snowShowersNight';
      case 'thunder':
        return 'thunderShowersNight';
      default:
        break;
    }
  }

  return icon;
}

const visualCrossingIconMap: Record<string, string> = {
  snow: 'snow',
  'snow-showers-day': 'snowShowersDay',
  'snow-showers-night': 'snowShowersNight',
  'thunder-rain': 'thunder',
  'thunder-showers-day': 'thunderShowersDay',
  'thunder-showers-night': 'thunderShowersNight',
  rain: 'rain',
  'showers-day': 'rainDay',
  'showers-night': 'rainNight',
  fog: 'fog',
  wind: 'windy',
  cloudy: 'cloudy',
  'partly-cloudy-day': 'partlyCloudyDay',
  'partly-cloudy-night': 'partlyCloudyNight',
  'clear-day': 'clearDay',
  'clear-night': 'clearNight',
};

/** Visual Crossing `icons2` name to an icon key; unknown names read as clear day. */
export function translateVisualCrossingIcon(icon: string): string {
  return Object.hasOwn(visualCrossingIconMap, icon) ? visualCrossingIconMap[icon] : 'clearDay';
}
