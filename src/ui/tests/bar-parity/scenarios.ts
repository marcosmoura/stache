/**
 * Maps each of the reference's 65 scenarios (`bar-reference/manifest.json` `scenarios`) to the data
 * the new Bar receives through the static fixture seam (`BarDataProvider`).
 *
 * The values restate the synthetic data the reference scenarios chose (`scripts/bar-reference/
 * scenarios.ts`); they are inputs, never readings. The coverage test fails when the reference gains
 * or loses a scenario that has no mapping here.
 */
import type { BarData } from '../../src/shell/windows/Bar/Bar.data';
import type { BatteryReading } from '../../src/shell/windows/Bar/Battery/Battery.types';
import type { MediaPayload } from '../../src/shell/windows/Bar/Media/Media.types';
import type { WifiInfo } from '../../src/shell/windows/Bar/Wifi/Wifi.types';
import { barData } from '../barFixtures';

/** How the new Bar is mounted for the scenario. */
export type ParityMode =
  | 'bar'
  /** A child throws: the root ErrorBoundary renders `QueryErrorFallback`. */
  | 'error'
  /** A child suspends: the root Suspense (fallback null) renders nothing. */
  | 'suspense'
  /** Spaces is suspended: `SpacesFallback` fills the first slot. */
  | 'spaces-fallback';

export interface ParityScenario {
  name: string;
  data: BarData;
  mode: ParityMode;
  /** `visible` = the system menu bar is on screen, the Bar is slid away. */
  menu: 'hidden' | 'visible';
  /** The reference shows the Surface fade at its initial frame. */
  fadeInitial?: boolean;
  /** Artwork must load into an `<img>` (true) or fail (false); absent = no artwork. */
  artwork?: 'loaded' | 'error';
}

const SPOTIFY = 'com.spotify.client';
const LONG_TITLE =
  'A Deliberately Very Long Synthetic Track Title That Exceeds The Maximum Label Width Of The Bar';

const media = (bundleIdentifier: string, overrides: Partial<MediaPayload> = {}): MediaPayload => ({
  title: 'Synthetic Song',
  artist: 'Synthetic Artist',
  bundleIdentifier,
  playing: true,
  ...overrides,
});

const baseData = (): BarData =>
  barData({
    cpu: { usage: 23, temperature: 48 },
    battery: { state: 'Discharging', percentage: 80 },
    keepAwake: false,
    wifi: { status: 'Connected', networkName: 'Home', signalStrength: -55 },
    weather: {
      isConfigured: true,
      isLoading: false,
      currentConditions: { feelslike: 21.2, icon: 'clearDay' },
    },
  });

const withData = (
  name: string,
  patch: Partial<BarData> = {},
  extra: Partial<ParityScenario> = {},
) =>
  ({
    name,
    data: { ...baseData(), ...patch },
    mode: 'bar',
    menu: 'hidden',
    ...extra,
  }) as ParityScenario;

const battery = (name: string, state: BatteryReading['state'], percentage: number) =>
  withData(name, { battery: { state, percentage } });
const cpu = (name: string, usage: number, temperature: number | null) =>
  withData(name, { cpu: { usage, temperature } });
const wifi = (name: string, info: WifiInfo) => withData(name, { wifi: info });
const connected = (signalStrength: number | null, networkName = 'Home'): WifiInfo => ({
  status: 'Connected',
  networkName,
  signalStrength,
});

const WEATHER_ICON_KEYS = [
  'snow',
  'snowShowersDay',
  'snowShowersNight',
  'thunder',
  'thunderShowersDay',
  'thunderShowersNight',
  'rain',
  'rainDay',
  'rainNight',
  'fog',
  'windy',
  'cloudy',
  'partlyCloudyDay',
  'partlyCloudyNight',
  'clearDay',
  'clearNight',
];

function throwing(): BarData {
  return {
    ...baseData(),
    weather: {
      get isConfigured(): boolean {
        throw new Error('Synthetic query failure');
      },
      isLoading: false,
    },
  };
}

function suspended(): BarData {
  return {
    ...baseData(),
    weather: {
      get isConfigured(): boolean {
        throw new Promise(() => {});
      },
      isLoading: false,
    },
  };
}

/** Artwork bytes are the reference's own synthetic PNG, injected by the suite (see `artworkFrom`). */
export function buildScenarios(artworkBase64: string): ParityScenario[] {
  return [
    withData('bar-default'),
    withData('bar-fade-initial', {}, { fadeInitial: true }),
    withData('menu-bar-hidden', {}, { menu: 'visible' }),
    { name: 'global-suspense', data: suspended(), mode: 'suspense', menu: 'hidden' },
    { name: 'global-error', data: throwing(), mode: 'error', menu: 'hidden' },
    withData('spaces-fallback', {}, { mode: 'spaces-fallback' }),

    withData('media-playing-spotify', { media: media(SPOTIFY) }),
    withData('media-paused', { media: media(SPOTIFY, { playing: false }) }),
    withData('media-no-artist', { media: media(SPOTIFY, { artist: '' }) }),
    withData('media-youtube-edge', { media: media('com.microsoft.edgemac.Dev') }),
    withData('media-tidal', { media: media('com.tidal.desktop') }),
    withData('media-feishin', { media: media('org.jeffvli.feishin') }),
    withData('media-unknown-player', { media: media('com.example.unknown') }),
    withData(
      'media-artwork-loaded',
      { media: media(SPOTIFY, { artwork: artworkBase64 }) },
      { artwork: 'loaded' },
    ),
    withData(
      'media-artwork-error',
      { media: media(SPOTIFY, { artwork: 'AAAA' }) },
      { artwork: 'error' },
    ),
    withData('media-long-title', { media: media(SPOTIFY, { title: LONG_TITLE }) }),
    withData('media-long-title-scrolling', { media: media(SPOTIFY, { title: LONG_TITLE }) }),

    withData('battery-absent', { battery: null }),
    battery('battery-full', 'Full', 100),
    battery('battery-unknown', 'Unknown', 55),
    battery('battery-charging', 'Charging', 60),
    battery('battery-discharging-75', 'Discharging', 75),
    battery('battery-discharging-50', 'Discharging', 50),
    battery('battery-discharging-25', 'Discharging', 25),
    battery('battery-empty', 'Empty', 10),
    // The seam types `state` as required; legacy rendered "Loading..." for a percentage without
    // one. The runtime branch exists, only the static type forbids the fixture (reported as a gap).
    withData('battery-label-loading', { battery: { percentage: 40 } as unknown as BatteryReading }),

    cpu('cpu-no-temperature', 23, null),
    cpu('cpu-temperature', 23, 48),
    cpu('cpu-temperature-zero', 5, 0),
    cpu('cpu-threshold-84', 40, 84),
    cpu('cpu-threshold-85', 40, 85),
    cpu('cpu-hot', 95, 97),

    wifi('wifi-unknown', { status: 'Unknown', networkName: null, signalStrength: null }),
    wifi('wifi-off', { status: 'Off', networkName: null, signalStrength: null }),
    wifi('wifi-disconnected', { status: 'Disconnected', networkName: null, signalStrength: null }),
    wifi('wifi-missing-ssid', { status: 'Connected', networkName: null, signalStrength: -55 }),
    wifi('wifi-signal-full', connected(-55)),
    wifi('wifi-signal-medium', connected(-65)),
    wifi('wifi-signal-low', connected(-75)),
    wifi('wifi-signal-weak', connected(-90)),
    wifi('wifi-signal-unknown', connected(null)),
    wifi(
      'wifi-long-ssid-scrolling',
      connected(-55, 'A Very Long Synthetic Network Name That Overflows'),
    ),

    withData('weather-loaded'),
    withData('weather-loading', { weather: { isConfigured: true, isLoading: true } }),
    withData('weather-error', { weather: { isConfigured: true, isLoading: false } }),
    withData('weather-unconfigured', { weather: { isConfigured: false, isLoading: false } }),

    withData('keep-awake-pending', { keepAwake: undefined }),
    withData('keep-awake-active', { keepAwake: true }),
    withData('keep-awake-inactive', { keepAwake: false }),

    ...WEATHER_ICON_KEYS.map((key) =>
      withData(`weather-icon-${key}`, {
        weather: {
          isConfigured: true,
          isLoading: false,
          currentConditions: { feelslike: 21.2, icon: key },
        },
      }),
    ),
  ];
}
