/**
 * Fixture scenarios: one entry per shipped legacy visual branch of the Bar.
 *
 * Each scenario only chooses data at the I/O boundary (query results, weather store state, the
 * media-query result). The legacy components compute everything else. Values are synthetic test
 * data, never captured native readings.
 */
import { deflateSync } from 'node:zlib';

import {
  FAIL,
  SUSPEND,
  type RenderContext,
  type ScrollState,
  type TilingFixture,
} from './stubs.ts';

export const FROZEN_NOW = '2026-11-10T14:05:09.000Z';
export const FROZEN_CLOCK_LABEL = 'Tue Nov 10 14:05:09';

const SPOTIFY = 'com.spotify.client';
const EDGE = 'com.microsoft.edgemac.Dev';
const TIDAL = 'com.tidal.desktop';
const FEISHIN = 'org.jeffvli.feishin';

/** Uncompressed (stored-block) PNG so the bytes do not depend on a zlib version. */
function syntheticArtwork(): string {
  const size = 48;
  const rows: Buffer[] = [];

  for (let y = 0; y < size; y++) {
    const row = Buffer.alloc(1 + size * 3);

    for (let x = 0; x < size; x++) {
      row[1 + x * 3] = Math.round((x / (size - 1)) * 255);
      row[2 + x * 3] = Math.round((y / (size - 1)) * 255);
      row[3 + x * 3] = 128;
    }

    rows.push(row);
  }

  const raw = Buffer.concat(rows);
  // `level: 0` emits stored blocks, which are fully specified by the DEFLATE format.
  const data = deflateSync(raw, { level: 0 });

  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;

    for (let k = 0; k < 8; k++) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }

    return c >>> 0;
  });
  const crc = (buffer: Buffer) => {
    let c = 0xffffffff;

    for (const byte of buffer) {
      c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
    }

    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (type: string, body: Buffer) => {
    const length = Buffer.alloc(4);

    length.writeUInt32BE(body.length);
    const checksum = Buffer.alloc(4);

    checksum.writeUInt32BE(crc(Buffer.concat([Buffer.from(type), body])));

    return Buffer.concat([length, Buffer.from(type), body, checksum]);
  };

  const header = Buffer.alloc(13);

  header.writeUInt32BE(size, 0);
  header.writeUInt32BE(size, 4);
  header[8] = 8;
  header[9] = 2;

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', header),
    chunk('IDAT', data),
    chunk('IEND', Buffer.alloc(0)),
  ]).toString('base64');
}

export const ARTWORK_BASE64 = syntheticArtwork();

export type ScenarioPatch = Partial<Omit<RenderContext, 'queries' | 'weather'>> & {
  queries?: Record<string, unknown>;
  weather?: Partial<RenderContext['weather']>;
  /** Marks the failure branch: rendered through `QueryErrorFallback`, see `extract.ts`. */
  errorFallback?: boolean;
};

export interface Scenario {
  name: string;
  description: string;
  patch: ScenarioPatch;
}

export const WEATHER_ICON_KEYS = [
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
] as const;

export function baseContext(): RenderContext {
  return {
    compact: false,
    queries: {
      'menubar-visibility': false,
      media: null,
      cpu: { usage: 23, temperature: 48 },
      battery: { state: 'Discharging', percentage: 80 },
      'keep-awake': false,
      wifi: { status: 'Connected', networkName: 'Home', signalStrength: -55 },
    },
    weather: {
      weather: { currentConditions: { feelslike: 21.2, icon: 'clearDay' } },
      isLoading: false,
      isConfigured: true,
    },
    tiling: null,
    surfaceOpacity: 1,
    loadedArtwork: undefined,
    scrollStates: [],
    scrollCursor: 0,
    now: FROZEN_NOW,
  };
}

const media = (
  bundleIdentifier: string,
  overrides: Record<string, unknown> = {},
): Record<string, unknown> => ({
  title: 'Synthetic Song',
  artist: 'Synthetic Artist',
  bundleIdentifier,
  playing: true,
  ...overrides,
});

const LONG_TITLE =
  'A Deliberately Very Long Synthetic Track Title That Exceeds The Maximum Label Width Of The Bar';
// Legacy measures these in the browser; the fixture fixes them (distance = wrapper + label width).
const SCROLL: ScrollState = { start: 560, end: -900, distance: 1460 };

const battery = (state: string, percentage: number) => ({
  queries: { battery: { state, percentage } },
});
const cpu = (usage: number, temperature: number | null) => ({
  queries: { cpu: { usage, temperature } },
});
const wifi = (info: Record<string, unknown>) => ({ queries: { wifi: info } });

export function buildScenarios(): Scenario[] {
  const scenarios: Scenario[] = [
    {
      name: 'bar-default',
      description: 'Settled Bar, no media, empty first slot (tiling absent).',
      patch: {},
    },
    {
      name: 'bar-fade-initial',
      description: 'Surface motion at its initial frame (opacity 0), before the 0.275s fade.',
      patch: { surfaceOpacity: 0 },
    },
    {
      name: 'menu-bar-hidden',
      description: 'menubar-visibility = true: bar hidden (translateY(100%), opacity 0).',
      patch: { queries: { 'menubar-visibility': true } },
    },
    {
      name: 'global-suspense',
      description: 'A status query is pending: the root Suspense (fallback null) renders nothing.',
      patch: { queries: { cpu: SUSPEND } },
    },
    {
      name: 'global-error',
      description: 'ErrorBoundary fallback (QueryErrorFallback) after a rejected query.',
      patch: { queries: { cpu: FAIL }, errorFallback: true },
    },
    {
      name: 'spaces-fallback',
      description: 'Spaces suspended: SpacesFallback "Loading..." Surface in the first slot.',
      patch: { queries: { tiling_workspace_data: SUSPEND } },
    },

    {
      name: 'media-playing-spotify',
      description: 'Playing Spotify, no artwork.',
      patch: { queries: { media: media(SPOTIFY) } },
    },
    {
      name: 'media-paused',
      description: 'Paused: prefix.',
      patch: { queries: { media: media(SPOTIFY, { playing: false }) } },
    },
    {
      name: 'media-no-artist',
      description: 'Title only (no artist).',
      patch: { queries: { media: media(SPOTIFY, { artist: '' }) } },
    },
    {
      name: 'media-youtube-edge',
      description: 'Edge Dev player icon.',
      patch: { queries: { media: media(EDGE) } },
    },
    {
      name: 'media-tidal',
      description: 'Tidal simple-icons player icon (size 18).',
      patch: { queries: { media: media(TIDAL) } },
    },
    {
      name: 'media-feishin',
      description: 'Feishin player icon.',
      patch: { queries: { media: media(FEISHIN) } },
    },
    {
      name: 'media-unknown-player',
      description: 'Unknown bundle: default PlayCircle02 icon.',
      patch: { queries: { media: media('com.example.unknown') } },
    },
    {
      name: 'media-artwork-loaded',
      description: 'Artwork preloaded: 24x24 radius-10 image before the icon.',
      patch: {
        queries: { media: media(SPOTIFY, { artwork: ARTWORK_BASE64 }) },
        loadedArtwork: `data:image/png;base64,${ARTWORK_BASE64}`,
      },
    },
    {
      name: 'media-artwork-error',
      description: 'Artwork present but failed to load (loadedArtwork null): no image.',
      patch: { queries: { media: media(SPOTIFY, { artwork: 'AAAA' }) }, loadedArtwork: null },
    },
    {
      name: 'media-long-title',
      description: 'Long title, label not (yet) scrolling: clipped at max-width 560/400px.',
      patch: { queries: { media: media(SPOTIFY, { title: LONG_TITLE }) } },
    },
    {
      name: 'media-long-title-scrolling',
      description: 'Long title with injected ScrollingLabel state (scroll-text animation).',
      patch: { queries: { media: media(SPOTIFY, { title: LONG_TITLE }) }, scrollStates: [SCROLL] },
    },

    {
      name: 'battery-absent',
      description: 'No battery (null): item omitted.',
      patch: { queries: { battery: null } },
    },
    { name: 'battery-full', description: 'Full: label 100%.', patch: battery('Full', 100) },
    {
      name: 'battery-unknown',
      description: 'Unknown state: label "55%".',
      patch: battery('Unknown', 55),
    },
    {
      name: 'battery-charging',
      description: 'Charging: green, BatteryCharging.',
      patch: battery('Charging', 60),
    },
    {
      name: 'battery-discharging-75',
      description: 'Discharging 75%: BatteryMedium02, yellow.',
      patch: battery('Discharging', 75),
    },
    {
      name: 'battery-discharging-50',
      description: 'Discharging 50%: BatteryMedium.',
      patch: battery('Discharging', 50),
    },
    {
      name: 'battery-discharging-25',
      description: 'Discharging 25%: BatteryLow.',
      patch: battery('Discharging', 25),
    },
    {
      name: 'battery-empty',
      description: 'Empty state 10%: red, BatteryEmpty.',
      patch: battery('Empty', 10),
    },
    {
      name: 'battery-label-loading',
      description: 'Percentage without state: "Loading..." label.',
      patch: { queries: { battery: { percentage: 40 } } },
    },

    { name: 'cpu-no-temperature', description: 'CPU without temperature.', patch: cpu(23, null) },
    {
      name: 'cpu-temperature',
      description: 'CPU with temperature below threshold.',
      patch: cpu(23, 48),
    },
    {
      name: 'cpu-temperature-zero',
      description: 'Temperature 0 (falsy): legacy renders a bare "0".',
      patch: cpu(5, 0),
    },
    { name: 'cpu-threshold-84', description: 'Just below the 85 threshold.', patch: cpu(40, 84) },
    {
      name: 'cpu-threshold-85',
      description: 'At the 85 threshold: red CpuCharge icon.',
      patch: cpu(40, 85),
    },
    { name: 'cpu-hot', description: 'Above threshold.', patch: cpu(95, 97) },

    {
      name: 'wifi-unknown',
      description: 'Unknown status: item omitted.',
      patch: wifi({ status: 'Unknown' }),
    },
    {
      name: 'wifi-off',
      description: 'Off: red WifiOff02, "Wi-Fi Off".',
      patch: wifi({ status: 'Off' }),
    },
    {
      name: 'wifi-disconnected',
      description: 'Disconnected: yellow, "Disconnected".',
      patch: wifi({ status: 'Disconnected' }),
    },
    {
      name: 'wifi-missing-ssid',
      description: 'Connected without SSID: icon only.',
      patch: wifi({ status: 'Connected', networkName: null, signalStrength: -55 }),
    },
    {
      name: 'wifi-signal-full',
      description: 'Signal -55 dBm.',
      patch: wifi({ status: 'Connected', networkName: 'Home', signalStrength: -55 }),
    },
    {
      name: 'wifi-signal-medium',
      description: 'Signal -65 dBm.',
      patch: wifi({ status: 'Connected', networkName: 'Home', signalStrength: -65 }),
    },
    {
      name: 'wifi-signal-low',
      description: 'Signal -75 dBm.',
      patch: wifi({ status: 'Connected', networkName: 'Home', signalStrength: -75 }),
    },
    {
      name: 'wifi-signal-weak',
      description: 'Signal -90 dBm: Wifi01.',
      patch: wifi({ status: 'Connected', networkName: 'Home', signalStrength: -90 }),
    },
    {
      name: 'wifi-signal-unknown',
      description: 'Connected, signal null: Wifi01.',
      patch: wifi({ status: 'Connected', networkName: 'Home', signalStrength: null }),
    },
    {
      name: 'wifi-long-ssid-scrolling',
      description: 'Long SSID with injected scrolling state.',
      patch: {
        ...wifi({
          status: 'Connected',
          networkName: 'A Very Long Synthetic Network Name That Overflows',
          signalStrength: -55,
        }),
        scrollStates: [{ start: 256, end: -400, distance: 656 }],
      },
    },

    { name: 'weather-loaded', description: 'Weather loaded.', patch: {} },
    {
      name: 'weather-loading',
      description: 'Configured, loading: "Loading...".',
      patch: { weather: { weather: undefined, isLoading: true } },
    },
    {
      name: 'weather-error',
      description: 'Configured, not loading, no data (failed): "Loading..." with default icon.',
      patch: { weather: { weather: undefined, isLoading: false } },
    },
    {
      name: 'weather-unconfigured',
      description: 'Provider not configured: item omitted.',
      patch: { weather: { isConfigured: false } },
    },

    {
      name: 'keep-awake-pending',
      description: 'Unresolved (data undefined): item omitted.',
      patch: { queries: { 'keep-awake': undefined } },
    },
    {
      name: 'keep-awake-active',
      description: 'Active: green fill and stroke.',
      patch: { queries: { 'keep-awake': true } },
    },
    {
      name: 'keep-awake-inactive',
      description: 'Inactive: transparent fill.',
      patch: { queries: { 'keep-awake': false } },
    },
  ];

  for (const key of WEATHER_ICON_KEYS) {
    scenarios.push({
      name: `weather-icon-${key}`,
      description: `Weather icon key ${key}.`,
      patch: { weather: { weather: { currentConditions: { feelslike: 21.2, icon: key } } } },
    });
  }

  scenarios.push(...stripScenarios());

  return scenarios;
}

// -- Tiling strip (Spaces) --------------------------------------------------------------------
//
// Synthetic Tiling answers at the `invoke` boundary. The pinned `fetchWorkspacesData` /
// `fetchAppsData` sort and map them and the pinned `useSpaces` derives display names, truncation and
// the laptop focused-only reduction; nothing here pre-computes what legacy renders.

/** Legacy `workspaceOrder`, given to Tiling reversed so the pinned sort has work to do. */
export const STRIP_WORKSPACES = [
  'terminal',
  'coding',
  'browser',
  'music',
  'design',
  'communication',
  'guitar',
  'misc',
  'files',
  'tasks',
] as const;

/**
 * Every app name of the legacy `appIcons` map (key order of the pinned source, including the
 * U+200E-prefixed WhatsApp), the seven approved additions (user decision 2026-11-10) and one unknown
 * name for the `DashboardCircleIcon` fallback.
 */
export const STRIP_APP_NAMES = [
  'Ableton Live',
  'Activity Monitor',
  'App Store',
  'Archetype Gojira X',
  'Archetype John Mayer X',
  'Archetype Nolly X',
  'Audio MIDI Setup',
  'Fortin Nameless Suite X',
  'Microsoft Edge Dev',
  'Proton Drive',
  'Proton Pass',
  'Proton VPN',
  'Soldano SLO100 X',
  '\u200EWhatsApp',
  'Zed Preview',
  'Bloom',
  'Code',
  'Dia',
  'Discord',
  'Feishin',
  'Figma',
  'Finder',
  'Ghostty',
  'Reminders',
  'Safari',
  'Settings',
  'Slack',
  'Spotify',
  'TIDAL',
  'Transmission',
  'WhatsApp',
  'Zoom',
  'Zed',
  'Aside',
  'Herdr',
  'DeviceHub',
  'AIOStreams',
  'Nuvio',
  'FigCraft',
  'Synthetic Unknown App',
] as const;

/** Apps per icon-coverage scenario (an unfocused app is icon-only, so ten fit every layout). */
export const STRIP_APPS_PER_SCENARIO = 10;

export const STRIP_LONG_TITLE = 'A deliberately long synthetic window title that is cut.ts';
export const STRIP_UNICODE_TITLE = 'Ünïcødé façade — 文件 — a long synthetic window title.md';

type WindowSpec = [appName: string, title: string];

function tiling(
  workspaces: readonly string[],
  focusedWorkspace: string | null,
  windows: readonly WindowSpec[] = [],
  focusedIndex: number | null = null,
  enabled = true,
): { tiling: TilingFixture } {
  return {
    tiling: {
      enabled,
      workspaces: [...workspaces],
      focusedWorkspace,
      windows: windows.map(([appName, title], index) => ({ id: 101 + index, appName, title })),
      focusedWindow: focusedIndex === null ? null : 101 + focusedIndex,
    },
  };
}

const ALL_WORKSPACES = [...STRIP_WORKSPACES].reverse();
const FOCUS_APPS: WindowSpec[] = [
  ['Ghostty', 'zsh'],
  ['Code', 'main.ts'],
  ['Safari', 'Start Page'],
];
const DUPLICATES: WindowSpec[] = [
  ['Code', STRIP_LONG_TITLE],
  ['Code', STRIP_UNICODE_TITLE],
  ['Code', ''],
  ['Safari', 'Start Page'],
];

function stripScenarios(): Scenario[] {
  const scenarios: Scenario[] = [
    {
      name: 'spaces-workspaces-no-focus',
      description: 'Every legacy workspace icon, none focused (no indicator), no apps.',
      patch: tiling(ALL_WORKSPACES, null),
    },
    ...STRIP_WORKSPACES.map((name) => ({
      name: `spaces-workspace-focused-${name}`,
      description: `Every legacy workspace icon; ${name} focused (indicator), the rest unfocused.`,
      patch: tiling(ALL_WORKSPACES, name),
    })),
    {
      name: 'spaces-unknown-workspace',
      description:
        'Unknown workspace names sort last alphabetically; the focused unknown one has the indicator and no icon (approved deviation).',
      patch: tiling(['workspace-2', 'coding', 'terminal', 'workspace-1'], 'workspace-1'),
    },
  ];

  for (let start = 0; start < STRIP_APP_NAMES.length; start += STRIP_APPS_PER_SCENARIO) {
    const names = STRIP_APP_NAMES.slice(start, start + STRIP_APPS_PER_SCENARIO);
    const part = start / STRIP_APPS_PER_SCENARIO + 1;

    scenarios.push({
      name: `spaces-app-icons-${part}`,
      description: `App icons ${start + 1}-${start + names.length} of the app map (unfocused, no focused app).`,
      patch: tiling(
        ['terminal'],
        'terminal',
        names.map((name) => [name, 'Window']),
      ),
    });
  }

  scenarios.push(
    {
      name: 'spaces-app-focused',
      description:
        'Focused app shows its label; laptop widths reduce the list to the focused app only.',
      patch: tiling(['coding', 'terminal'], 'coding', FOCUS_APPS, 1),
    },
    {
      name: 'spaces-two-apps',
      description:
        'The spaces-app-focused strip after its last app (Safari) closed: the app exit end state.',
      patch: tiling(['coding', 'terminal'], 'coding', FOCUS_APPS.slice(0, 2), 1),
    },
    {
      name: 'spaces-duplicate-titles',
      description:
        'Duplicate app names show window titles; the focused long title is cut at 40 + "..." (laptop: focused only, app name).',
      patch: tiling(['coding', 'terminal'], 'coding', DUPLICATES, 0),
    },
    {
      name: 'spaces-duplicate-unicode',
      description: 'Duplicate app names; the focused Unicode title is cut at 40 + "...".',
      patch: tiling(['coding', 'terminal'], 'coding', DUPLICATES, 1),
    },
    {
      name: 'spaces-duplicate-no-focus',
      description:
        'Duplicate app names without a focused app: titles are derived (25 on laptop widths) but no label is drawn.',
      patch: tiling(['coding', 'terminal'], 'coding', DUPLICATES),
    },
    {
      name: 'spaces-absent',
      description: 'Tiling not enabled: the empty first-slot div in the full Bar.',
      patch: tiling(ALL_WORKSPACES, 'terminal', FOCUS_APPS, 0, false),
    },
    {
      name: 'spaces-empty',
      description: 'Tiling enabled with zero workspaces: the empty first-slot div in the full Bar.',
      patch: tiling([], null),
    },
    {
      name: 'spaces-with-media',
      description: 'Strip, centred media and status together in the full Bar layout.',
      patch: {
        ...tiling(['coding', 'terminal'], 'coding', FOCUS_APPS, 1),
        queries: { media: media(SPOTIFY) },
      },
    },
  );

  return scenarios;
}

export function contextFor(patch: ScenarioPatch, compact: boolean): RenderContext {
  const base = baseContext();
  const { queries, weather, errorFallback: _errorFallback, ...rest } = patch;

  return {
    ...base,
    ...rest,
    compact,
    queries: { ...base.queries, ...queries },
    weather: { ...base.weather, ...weather },
    scrollStates: [...(rest.scrollStates ?? [])],
    scrollCursor: 0,
  };
}
