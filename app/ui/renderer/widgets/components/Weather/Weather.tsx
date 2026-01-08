import {
  CloudAngledRainIcon,
  CloudAngledZapIcon,
  Location01Icon,
  SnowIcon,
  Sun03Icon,
  SunIcon,
} from '@hugeicons/core-free-icons';

import { Button } from '@/components/Button';
import { Icon } from '@/components/Icon';
import { Surface } from '@/components/Surface';
import { colors } from '@/design-system';

import {
  useWeatherWidget,
  type HourlyRainData,
  type NextPrecipitationEvent,
  type WeatherStat,
} from './Weather.state';
import * as styles from './Weather.styles';

interface CircularProgressProps {
  percentage: number;
  color: string;
  size: number;
  strokeWidth: number;
  children: React.ReactNode;
  className?: string;
  backgroundClass: string;
  fillClass: string;
  progressClass: string;
}

const CircularProgress = ({
  percentage,
  color,
  size,
  strokeWidth,
  children,
  className,
  backgroundClass,
  fillClass,
  progressClass,
}: CircularProgressProps) => {
  const radius = (size - strokeWidth) / 2;
  const circumference = 2 * Math.PI * radius;
  const offset = circumference - (percentage / 100) * circumference;

  return (
    <div className={className} style={{ width: size, height: size }}>
      <svg className={progressClass} width={size} height={size}>
        <circle className={backgroundClass} cx={size / 2} cy={size / 2} r={radius} />
        <circle
          className={fillClass}
          cx={size / 2}
          cy={size / 2}
          r={radius}
          stroke={color}
          strokeDasharray={circumference}
          strokeDashoffset={offset}
        />
      </svg>
      {children}
    </div>
  );
};

interface StatCardProps {
  stat: WeatherStat;
}

const StatCard = ({ stat }: StatCardProps) => {
  return (
    <div className={styles.statCard}>
      <CircularProgress
        percentage={stat.percentage}
        color={stat.color}
        size={48}
        strokeWidth={4}
        className={styles.statIndicator}
        backgroundClass={styles.statCircleBackground}
        fillClass={styles.statCircleFill}
        progressClass={styles.circularProgress}
      >
        <span className={styles.statValueInCircle}>{stat.displayValue}</span>
      </CircularProgress>
      <div className={styles.statInfo}>
        <span className={styles.statLabel}>{stat.label}</span>
        <div className={styles.statStatus} style={{ color: stat.color }}>
          <span className={styles.statusDot} style={{ backgroundColor: stat.color }} />
          {stat.status}
        </div>
        <span className={styles.statDetail}>
          {stat.displayValue} {stat.unit}
        </span>
      </div>
    </div>
  );
};

interface RainForecastProps {
  forecast: HourlyRainData[];
  nextPrecipitation: NextPrecipitationEvent | null;
}

const getPrecipIcon = (precipType: 'rain' | 'snow' | 'mixed') => {
  switch (precipType) {
    case 'snow':
      return SnowIcon;
    case 'mixed':
      return CloudAngledZapIcon;
    default:
      return CloudAngledRainIcon;
  }
};

const getPrecipTypeLabel = (precipType: 'rain' | 'snow' | 'mixed') => {
  switch (precipType) {
    case 'snow':
      return 'Snow';
    case 'mixed':
      return 'Rain & Snow';
    default:
      return 'Rain';
  }
};

const getPrecipColors = (precipType: 'rain' | 'snow' | 'mixed') => {
  switch (precipType) {
    case 'snow':
      return { bg: 'rgba(137, 220, 235, 0.15)', color: colors.sky };
    case 'mixed':
      return { bg: 'rgba(180, 190, 254, 0.15)', color: colors.lavender };
    default:
      return { bg: 'rgba(116, 199, 236, 0.15)', color: colors.sapphire };
  }
};

const RainForecast = ({ forecast, nextPrecipitation }: RainForecastProps) => {
  const hasRainChance = forecast.some((hour) => hour.precipProb > 10);

  return (
    <div className={styles.rainForecastSection}>
      <div className={styles.rainForecastHeader}>
        <Icon icon={CloudAngledRainIcon} size={18} className={styles.rainForecastHeaderIcon} />
        <span>Precipitation Forecast</span>
      </div>

      {!hasRainChance ? (
        nextPrecipitation ? (
          <NextPrecipitationCard event={nextPrecipitation} />
        ) : (
          <div className={styles.clearSkiesCard}>
            <div className={styles.clearSkiesIcon}>
              <Icon icon={Sun03Icon} size={20} />
            </div>
            <div className={styles.clearSkiesInfo}>
              <span className={styles.clearSkiesTitle}>Clear skies ahead</span>
              <span className={styles.clearSkiesDetails}>
                No precipitation expected in the next 5 days
              </span>
            </div>
          </div>
        )
      ) : (
        <>
          <div className={styles.rainForecastChart}>
            {forecast.map((hour) => (
              <div key={hour.time} className={styles.rainForecastBar}>
                <div className={styles.rainBar}>
                  <div
                    className={styles.rainBarFill}
                    style={{
                      height: `${Math.max(hour.precipProb, 5)}%`,
                      backgroundColor: hour.color,
                    }}
                  />
                </div>
                <span className={styles.rainBarValue}>{Math.round(hour.precipProb)}%</span>
                <span className={styles.rainBarLabel}>{hour.hour}</span>
              </div>
            ))}
          </div>

          <div className={styles.rainForecastLegend}>
            <div className={styles.legendItem}>
              <span className={styles.legendDot} style={{ backgroundColor: '#a6e3a1' }} />
              <span>Low</span>
            </div>
            <div className={styles.legendItem}>
              <span className={styles.legendDot} style={{ backgroundColor: '#74c7ec' }} />
              <span>Moderate</span>
            </div>
            <div className={styles.legendItem}>
              <span className={styles.legendDot} style={{ backgroundColor: '#f9e2af' }} />
              <span>High</span>
            </div>
            <div className={styles.legendItem}>
              <span className={styles.legendDot} style={{ backgroundColor: '#89dceb' }} />
              <span>Very High</span>
            </div>
          </div>
        </>
      )}
    </div>
  );
};

interface NextPrecipitationCardProps {
  event: NextPrecipitationEvent;
}

const NextPrecipitationCard = ({ event }: NextPrecipitationCardProps) => {
  const precipColors = getPrecipColors(event.precipType);
  const PrecipIcon = getPrecipIcon(event.precipType);

  return (
    <div className={styles.nextPrecipCard}>
      <div
        className={styles.nextPrecipIcon}
        style={{ backgroundColor: precipColors.bg, color: precipColors.color }}
      >
        <Icon icon={PrecipIcon} size={20} />
      </div>
      <div className={styles.nextPrecipInfo}>
        <div className={styles.nextPrecipTitle}>
          <span>
            {getPrecipTypeLabel(event.precipType)} expected {event.dayName.toLowerCase()}
          </span>
        </div>
        <span className={styles.nextPrecipDetails}>
          {event.date} • {event.conditions} • {event.tempMin}° / {event.tempMax}°
        </span>
      </div>
      <span className={styles.nextPrecipProb}>{Math.round(event.precipProb)}%</span>
    </div>
  );
};

export const Weather = () => {
  const { weather, onOpenWeatherClick } = useWeatherWidget();

  if (!weather) {
    return (
      <Surface className={styles.weather}>
        <div className={styles.header}>
          <div className={styles.headerContent}>
            <div className={styles.headerTitle}>Today&apos;s weather</div>
            <div className={styles.headerSubtitle}>Loading weather data...</div>
          </div>
        </div>
        <div className={styles.mainDisplay}>
          <div className={styles.temperatureDisplay}>
            <span className={styles.temperatureValue}>--°</span>
          </div>
          <div className={styles.mainInfo}>
            <span className={styles.statusLabel}>Loading...</span>
            <span className={styles.statusDescription}>
              Weather data is being fetched. Please wait a moment.
            </span>
          </div>
        </div>
      </Surface>
    );
  }

  const {
    temperature,
    feelsLike,
    tempColor,
    tempStatus,
    tempDescription,
    icon: WeatherIcon,
    location,
    stats,
    hourlyRainForecast,
    nextPrecipitation,
  } = weather;

  return (
    <Surface className={styles.weather}>
      {/* Header */}
      <div className={styles.header}>
        <div className={styles.headerContent}>
          <div className={styles.headerTitle}>Today&apos;s weather</div>
          <div className={styles.headerSubtitle}>
            <Icon icon={Location01Icon} size={12} />
            <span>{location}</span>
          </div>
        </div>
        <Button type="button" aria-label="Open weather" onClick={onOpenWeatherClick}>
          Open Weather App
        </Button>
      </div>

      {/* Main temperature display */}
      <div className={styles.mainDisplay}>
        <div className={styles.temperatureDisplay}>
          <span className={styles.temperatureValue} style={{ color: tempColor }}>
            {temperature ?? '--'}°
          </span>
          {feelsLike != null && feelsLike !== temperature && (
            <span className={styles.temperatureFeelsLike}>Feels like {feelsLike}°</span>
          )}
        </div>
        <div className={styles.mainInfo}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <Icon icon={WeatherIcon || SunIcon} size={24} color={tempColor} />
            <span className={styles.statusLabel} style={{ color: tempColor }}>
              {tempStatus}
            </span>
          </div>
          <span className={styles.statusDescription}>{tempDescription}</span>
        </div>
      </div>

      {/* Rain Forecast */}
      {hourlyRainForecast.length > 0 && (
        <RainForecast forecast={hourlyRainForecast} nextPrecipitation={nextPrecipitation} />
      )}

      <div className={styles.statsGrid}>
        {stats.map((stat) => (
          <StatCard key={stat.id} stat={stat} />
        ))}
      </div>
    </Surface>
  );
};
