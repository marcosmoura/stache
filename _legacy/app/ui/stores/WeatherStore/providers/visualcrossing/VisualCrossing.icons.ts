const providerIconMap: Record<string, string> = {
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

export const translateIcon = (icon: string): string => {
  return providerIconMap[icon] ?? 'clearDay';
};
