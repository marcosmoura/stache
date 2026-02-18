const providerIconMap: Record<number, string> = {
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

export const translateIcon = (weatherCode: number, isDay: boolean = true): string => {
  const icon = providerIconMap[weatherCode] ?? 'clearDay';

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
};

export const getWeatherCondition = (weatherCode: number): string => {
  const conditions: Record<number, string> = {
    0: 'Clear',
    1: 'Mainly Clear',
    2: 'Partly Cloudy',
    3: 'Overcast',
    45: 'Fog',
    48: 'Depositing Rime Fog',
    51: 'Light Drizzle',
    53: 'Moderate Drizzle',
    55: 'Dense Drizzle',
    56: 'Light Freezing Drizzle',
    57: 'Dense Freezing Drizzle',
    61: 'Slight Rain',
    63: 'Moderate Rain',
    65: 'Heavy Rain',
    66: 'Light Freezing Rain',
    67: 'Heavy Freezing Rain',
    71: 'Slight Snow',
    73: 'Moderate Snow',
    75: 'Heavy Snow',
    77: 'Snow Grains',
    80: 'Slight Rain Showers',
    81: 'Moderate Rain Showers',
    82: 'Violent Rain Showers',
    85: 'Slight Snow Showers',
    86: 'Heavy Snow Showers',
    95: 'Thunderstorm',
    96: 'Thunderstorm with Slight Hail',
    99: 'Thunderstorm with Heavy Hail',
  };

  return conditions[weatherCode] ?? 'Unknown';
};

export const getPrecipType = (weatherCode: number): string[] | null => {
  const rainCodes = [51, 53, 55, 56, 57, 61, 63, 65, 66, 67, 80, 81, 82];
  const snowCodes = [71, 73, 75, 77, 85, 86];
  const thunderCodes = [95, 96, 99];

  if (thunderCodes.includes(weatherCode)) {
    return ['thunderstorm'];
  }

  if (snowCodes.includes(weatherCode)) {
    return ['snow'];
  }

  if (rainCodes.includes(weatherCode)) {
    return ['rain'];
  }

  return null;
};
