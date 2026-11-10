import type { AnyIcon } from '../../../components/Icon';

/** The fields of the legacy weather store the Bar renders. */
export type WeatherReading = {
  /** Legacy `isProviderAvailable(config)`; an unconfigured provider hides the item. */
  isConfigured: boolean;
  /** Legacy query `isLoading`. */
  isLoading: boolean;
  /** Legacy `weather.currentConditions`, reduced to the rendered fields. */
  currentConditions?: {
    /** Feels-like temperature in Celsius; `null` renders as 0. */
    feelslike: number | null;
    /** Normalized icon key, for example `partlyCloudyDay`. */
    icon: string;
  };
};

export type WeatherState = {
  label: string;
  icon: AnyIcon;
  isConfigured: boolean;
};
