import { useCallback, useMemo } from 'react';

import { invoke } from '@tauri-apps/api/core';

import { colors } from '@/design-system';
import { getWeatherIcon, useWeatherStore } from '@/stores/WeatherStore';

function getWindDirection(degrees: number | null | undefined): string {
  if (degrees == null) return '';
  const directions = ['N', 'NE', 'E', 'SE', 'S', 'SW', 'W', 'NW'];
  const index = Math.round(degrees / 45) % 8;
  return directions[index];
}

function getMoonPhaseLabel(phase: number): string {
  if (phase === 0 || phase === 1) return 'New Moon';
  if (phase < 0.25) return 'Waxing Crescent';
  if (phase === 0.25) return 'First Quarter';
  if (phase < 0.5) return 'Waxing Gibbous';
  if (phase === 0.5) return 'Full Moon';
  if (phase < 0.75) return 'Waning Gibbous';
  if (phase === 0.75) return 'Last Quarter';
  return 'Waning Crescent';
}

function getTemperatureColor(temp: number | null | undefined): string {
  if (temp == null) return colors.text;
  if (temp <= 0) return colors.sky;
  if (temp <= 10) return colors.sapphire;
  if (temp <= 20) return colors.green;
  if (temp <= 30) return colors.yellow;
  if (temp <= 35) return colors.peach;
  return colors.red;
}

function getTemperatureStatus(temp: number | null | undefined): string {
  if (temp == null) return 'Unknown';
  if (temp <= 0) return 'Freezing';
  if (temp <= 10) return 'Cold';
  if (temp <= 18) return 'Cool';
  if (temp <= 24) return 'Pleasant';
  if (temp <= 30) return 'Warm';
  if (temp <= 35) return 'Hot';
  return 'Very Hot';
}

function getTemperatureDescription(temp: number | null | undefined, conditions: string): string {
  if (temp == null) return 'Weather data is being loaded...';

  const status = getTemperatureStatus(temp);
  const conditionsLower = conditions.toLowerCase();

  if (temp <= 0) {
    return `It's freezing outside with ${conditionsLower}. Bundle up warmly and be careful of icy conditions.`;
  }
  if (temp <= 10) {
    return `${status} temperature with ${conditionsLower}. A warm jacket is recommended for outdoor activities.`;
  }
  if (temp <= 18) {
    return `${status} and comfortable with ${conditionsLower}. Light layers should keep you comfortable.`;
  }
  if (temp <= 24) {
    return `${status} weather with ${conditionsLower}. Ideal conditions for most outdoor activities.`;
  }
  if (temp <= 30) {
    return `${status} conditions with ${conditionsLower}. Stay hydrated and seek shade when needed.`;
  }
  return `${status} temperature with ${conditionsLower}. Limit outdoor exposure and stay cool.`;
}

function getStatStatus(
  value: number | null | undefined,
  type: 'humidity' | 'wind' | 'visibility' | 'cloudCover' | 'pressure',
): { label: string; color: string } {
  if (value == null) return { label: 'N/A', color: colors.overlay1 };

  switch (type) {
    case 'humidity':
      if (value <= 30) return { label: 'Low', color: colors.yellow };
      if (value <= 60) return { label: 'Comfortable', color: colors.green };
      if (value <= 80) return { label: 'Humid', color: colors.peach };
      return { label: 'Very Humid', color: colors.red };

    case 'wind':
      if (value <= 10) return { label: 'Calm', color: colors.green };
      if (value <= 25) return { label: 'Breezy', color: colors.sapphire };
      if (value <= 40) return { label: 'Windy', color: colors.yellow };
      return { label: 'Strong', color: colors.red };

    case 'visibility':
      if (value >= 10) return { label: 'Excellent', color: colors.green };
      if (value >= 5) return { label: 'Good', color: colors.sapphire };
      if (value >= 2) return { label: 'Moderate', color: colors.yellow };
      return { label: 'Poor', color: colors.red };

    case 'cloudCover':
      if (value <= 20) return { label: 'Clear', color: colors.sapphire };
      if (value <= 50) return { label: 'Partly Cloudy', color: colors.green };
      if (value <= 80) return { label: 'Mostly Cloudy', color: colors.yellow };
      return { label: 'Overcast', color: colors.overlay1 };

    case 'pressure':
      if (value < 1000) return { label: 'Low', color: colors.yellow };
      if (value <= 1020) return { label: 'Normal', color: colors.green };
      return { label: 'High', color: colors.sapphire };

    default:
      return { label: 'N/A', color: colors.overlay1 };
  }
}

export interface WeatherStat {
  id: string;
  label: string;
  value: number | null;
  displayValue: string;
  unit: string;
  status: string;
  color: string;
  percentage: number;
}

export interface HourlyRainData {
  id: string;
  hour: string;
  time: string;
  precipProb: number;
  precip: number;
  precipType: string | null;
  icon: string;
  color: string;
}

export interface NextPrecipitationEvent {
  date: string;
  dayName: string;
  precipProb: number;
  precipType: 'rain' | 'snow' | 'mixed';
  precip: number;
  snow: number;
  conditions: string;
  icon: string;
  tempMax: number;
  tempMin: number;
  isToday: boolean;
  isTomorrow: boolean;
  daysFromNow: number;
}

function getPrecipColor(precipProb: number): string {
  if (precipProb <= 10) return colors.green;
  if (precipProb <= 30) return colors.sapphire;
  if (precipProb <= 50) return colors.yellow;
  if (precipProb <= 70) return colors.peach;
  return colors.sky;
}

function formatHour(date: Date): { hour: string; time: string } {
  const hour = date.getHours();
  const period = hour >= 12 ? 'PM' : 'AM';
  const displayHour = hour % 12 || 12;
  return {
    hour: `${displayHour}`,
    time: `${displayHour}:00 ${period}`,
  };
}

function getDayName(dateStr: string, daysFromNow: number): string {
  if (daysFromNow === 0) return 'Today';
  if (daysFromNow === 1) return 'Tomorrow';

  const date = new Date(dateStr);
  return date.toLocaleDateString('en-US', { weekday: 'long' });
}

function formatDate(dateStr: string): string {
  const date = new Date(dateStr);
  return date.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
}

export function useWeatherWidget() {
  const { weather } = useWeatherStore();

  const formattedData = useMemo(() => {
    if (!weather?.currentConditions) {
      return null;
    }

    const { currentConditions, resolvedAddress } = weather;
    const {
      temp,
      feelslike,
      humidity,
      windspeed,
      winddir,
      pressure,
      visibility,
      cloudcover,
      conditions,
      icon,
      moonphase,
    } = currentConditions;

    const weatherIcon = getWeatherIcon(icon);
    const tempColor = getTemperatureColor(feelslike);
    const tempStatus = getTemperatureStatus(feelslike);
    const tempDescription = getTemperatureDescription(feelslike, conditions);

    const windDirection = getWindDirection(winddir);

    // Calculate temperature percentage for the circular indicator (0-50°C range)
    const tempPercentage = temp != null ? Math.min(Math.max((temp + 10) / 60, 0), 1) * 100 : 0;

    const humidityStatus = getStatStatus(humidity, 'humidity');
    const windStatus = getStatStatus(windspeed, 'wind');
    const visibilityStatus = getStatStatus(visibility, 'visibility');
    const cloudStatus = getStatStatus(cloudcover, 'cloudCover');
    const pressureStatus = getStatStatus(pressure, 'pressure');

    const stats: WeatherStat[] = [
      {
        id: 'humidity',
        label: 'Humidity',
        value: humidity,
        displayValue: humidity != null ? Math.round(humidity).toString() : 'N/A',
        unit: '%',
        status: humidityStatus.label,
        color: humidityStatus.color,
        percentage: humidity ?? 0,
      },
      {
        id: 'wind',
        label: `Wind${windDirection ? ` (${windDirection})` : ''}`,
        value: windspeed,
        displayValue: windspeed != null ? Math.round(windspeed).toString() : 'N/A',
        unit: 'km/h',
        status: windStatus.label,
        color: windStatus.color,
        percentage: windspeed != null ? Math.min((windspeed / 60) * 100, 100) : 0,
      },
      {
        id: 'visibility',
        label: 'Visibility',
        value: visibility,
        displayValue: visibility != null ? Math.round(visibility).toString() : 'N/A',
        unit: 'km',
        status: visibilityStatus.label,
        color: visibilityStatus.color,
        percentage: visibility != null ? Math.min((visibility / 20) * 100, 100) : 0,
      },
      {
        id: 'cloudCover',
        label: 'Cloud Cover',
        value: cloudcover,
        displayValue: cloudcover != null ? Math.round(cloudcover).toString() : 'N/A',
        unit: '%',
        status: cloudStatus.label,
        color: cloudStatus.color,
        percentage: cloudcover ?? 0,
      },
      {
        id: 'pressure',
        label: 'Pressure',
        value: pressure,
        displayValue: pressure != null ? Math.round(pressure).toString() : 'N/A',
        unit: 'hPa',
        status: pressureStatus.label,
        color: pressureStatus.color,
        percentage: pressure != null ? Math.min(((pressure - 950) / 100) * 100, 100) : 0,
      },
      {
        id: 'moonPhase',
        label: 'Moon Phase',
        value: moonphase,
        displayValue: Number.isFinite(moonphase) ? Math.round(moonphase * 100).toString() : 'N/A',
        unit: '%',
        status: getMoonPhaseLabel(Number.isFinite(moonphase) ? moonphase : 0),
        color: colors.lavender,
        percentage: Number.isFinite(moonphase) ? moonphase * 100 : 0,
      },
    ];

    // Providers use either ISO timestamps or a day plus a time-only value.
    // Normalize both formats before selecting the next twelve forecast hours.
    const currentHour = new Date();
    currentHour.setMinutes(0, 0, 0);
    const hourlyRainForecast: HourlyRainData[] = (weather.days ?? [])
      .flatMap((day) =>
        day.hours.map((hour) => ({
          hour,
          date: new Date(
            hour.datetime.includes('T') ? hour.datetime : `${day.datetime}T${hour.datetime}`,
          ),
        })),
      )
      .filter(({ date }) => Number.isFinite(date.getTime()) && date >= currentHour)
      .sort((a, b) => a.date.getTime() - b.date.getTime())
      .slice(0, 12)
      .map(({ hour, date }) => {
        const { hour: displayHour, time } = formatHour(date);
        const precipProb = hour.precipprob ?? 0;
        return {
          id: date.toISOString(),
          hour: displayHour,
          time,
          precipProb,
          precip: hour.precip ?? 0,
          precipType: hour.preciptype?.[0] ?? null,
          icon: hour.icon,
          color: getPrecipColor(precipProb),
        };
      });

    // Find next precipitation event (rain or snow) in the next 5 days
    const days = weather.days ?? [];
    let nextPrecipitation: NextPrecipitationEvent | null = null;

    for (let i = 0; i < days.length; i++) {
      const day = days[i];
      const precipProb = day.precipprob ?? 0;
      const hasSnow = (day.snow ?? 0) > 0 || day.preciptype?.includes('snow');
      const hasRain = day.preciptype?.includes('rain') || ((day.precip ?? 0) > 0 && !hasSnow);

      // Consider significant precipitation (>20% chance)
      if (precipProb >= 20 && (hasRain || hasSnow)) {
        let precipType: 'rain' | 'snow' | 'mixed';
        if (hasSnow && hasRain) {
          precipType = 'mixed';
        } else if (hasSnow) {
          precipType = 'snow';
        } else {
          precipType = 'rain';
        }

        nextPrecipitation = {
          date: formatDate(day.datetime),
          dayName: getDayName(day.datetime, i),
          precipProb,
          precipType,
          precip: day.precip ?? 0,
          snow: day.snow ?? 0,
          conditions: day.conditions,
          icon: day.icon,
          tempMax: Math.round(day.tempmax),
          tempMin: Math.round(day.tempmin),
          isToday: i === 0,
          isTomorrow: i === 1,
          daysFromNow: i,
        };
        break;
      }
    }

    return {
      temperature: temp != null ? Math.round(temp) : null,
      feelsLike: feelslike != null ? Math.round(feelslike) : null,
      tempColor,
      tempStatus,
      tempDescription,
      tempPercentage,
      conditions: conditions || 'Unknown',
      icon: weatherIcon,
      location: resolvedAddress,
      stats,
      hourlyRainForecast,
      nextPrecipitation,
    };
  }, [weather]);

  const onOpenWeatherClick = useCallback(() => {
    invoke('open_app', { name: 'Weather' });
  }, []);

  return {
    weather: formattedData,
    onOpenWeatherClick,
  };
}
