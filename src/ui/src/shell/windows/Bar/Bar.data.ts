import { createContext, use } from 'react';

import type { BarSnapshotOutcome } from '../../../sdk';

import type { BatteryReading } from './Battery/Battery.types';
import type { CPUInfo } from './Cpu/Cpu.types';
import type { MediaPayload } from './Media/Media.types';
import { spacesFromTiling } from './Spaces/Spaces.data';
import type { SpacesStrip } from './Spaces/Spaces.types';
import type { WeatherReading } from './Weather/Weather.types';
import type { WifiInfo } from './Wifi/Wifi.types';

/** The host session facts a Keep Awake click is based on (never drawn). */
export type KeepAwakeSession = {
  /** The module's session pause flag as last observed. */
  paused: boolean;
  /** The module's accepted `enabled` flag as last observed. */
  enabled: boolean;
};

/**
 * The data the Bar components render, one field per provider.
 *
 * This is the static seam of part 04. Every provider-backed field starts
 * absent, and each component renders the branch legacy rendered for absent data.
 * Parts 05-08 replace a field's source (a generated, query-backed hook) and
 * never the component styles. Tests supply fixtures through `BarDataProvider`;
 * there is no fabricated native reading in production.
 *
 * `undefined` means unresolved (no provider yet); `null` is a resolved
 * "nothing there" where legacy distinguished the two.
 */
export type BarData = {
  /** Now playing; `null` hides the Media overlay. */
  media: MediaPayload | null;
  /** Weather store state. */
  weather: WeatherReading;
  /** CPU reading; `undefined` renders nothing, `null` is legacy's no-data branch. */
  cpu: CPUInfo | null | undefined;
  /** Battery reading; `null`/`undefined` hide the item. */
  battery: BatteryReading | null | undefined;
  /** Keep Awake confirmed state; `undefined` is legacy's null branch. */
  keepAwake: boolean | undefined;
  /** What the Keep Awake click is based on; `undefined` while unresolved. */
  keepAwakeSession?: KeepAwakeSession | undefined;
  /** Wi-Fi reading; `null`/`undefined` read as legacy `Unknown` and hide the item. */
  wifi: WifiInfo | null | undefined;
  /**
   * The Tiling strip from `BarView.tiling`; `null`/`undefined` is unresolved and,
   * like `absent`, renders the empty slot.
   */
  spaces?: SpacesStrip | null | undefined;
};

/** What production shows until a provider has a reading (and for every non-`available` answer). */
export const ABSENT_BAR_DATA: BarData = {
  media: null,
  weather: { isConfigured: false, isLoading: false },
  cpu: undefined,
  battery: undefined,
  keepAwake: undefined,
  wifi: undefined,
};

/**
 * `null` means no explicit data was supplied: production then renders the
 * bridge data (`barDataFromOutcome`). Tests and the parity gate supply fixtures
 * through `BarDataProvider`, which always wins.
 */
export const BarDataContext = createContext<BarData | null>(null);

export function useBarData(): BarData {
  return use(BarDataContext) ?? ABSENT_BAR_DATA;
}

/**
 * Projects the bridge answer onto the seam. The CPU, battery and Wi-Fi fields
 * come from the generated `BarView`; a provider that has not produced a reading
 * is absent, so each component renders legacy's unresolved branch. Media comes from
 * `BarView.media` without artwork (the artwork is fetched by id, see `useMediaArtwork`);
 * Keep Awake maps `BarView.keepAwake` (`null`/absent is unresolved, so the component renders its
 * legacy loading branch; otherwise only `active` is drawn). Spaces maps `BarView.tiling` into
 * legacy-shaped strip data (sorted workspaces, apps keyed by the opaque window key).
 */
export function barDataFromOutcome(outcome: BarSnapshotOutcome | undefined): BarData {
  if (outcome?.kind !== 'available') {
    return ABSENT_BAR_DATA;
  }

  const { cpu, battery, wifi, media, keepAwake, tiling } = outcome.view;

  return {
    ...ABSENT_BAR_DATA,
    media: media
      ? {
          title: media.title,
          artist: media.artist,
          album: media.album ?? null,
          bundleIdentifier: media.bundleIdentifier,
          mediaType: media.mediaType ?? null,
          playing: media.playing,
        }
      : null,
    cpu: cpu ? { usage: cpu.usage, temperature: cpu.temperature ?? null } : undefined,
    battery: battery ? { percentage: battery.percentage, state: battery.state } : undefined,
    wifi: wifi
      ? {
          status: wifi.status,
          networkName: wifi.networkName ?? null,
          signalStrength: wifi.signalStrength ?? null,
        }
      : undefined,
    keepAwake: keepAwake ? keepAwake.active : undefined,
    keepAwakeSession: keepAwake
      ? { paused: keepAwake.paused, enabled: keepAwake.enabled }
      : undefined,
    spaces: spacesFromTiling(tiling),
  };
}
