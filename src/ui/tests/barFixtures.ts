import type { BarSnapshotOutcome, BarView_Serialize } from '../src/sdk/bindings.gen';
import type { BarData } from '../src/shell/windows/Bar/Bar.data';

/** The data production shows before the providers of parts 05-08 land. */
export const absentBarData: BarData = {
  media: null,
  weather: { isConfigured: false, isLoading: false },
  cpu: undefined,
  battery: undefined,
  keepAwake: undefined,
  wifi: undefined,
};

/** A Bar data set; every field defaults to absent. */
export function barData(overrides: Partial<BarData> = {}): BarData {
  return { ...absentBarData, ...overrides };
}

/** A 1x1 transparent PNG, base64, that `Image` decodes. */
export const PNG_PIXEL =
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==';

export function barView(overrides: Partial<BarView_Serialize> = {}): BarView_Serialize {
  return {
    generation: '1',
    sequence: '1',
    configRevision: '1',
    panel: 'loaded',
    menu: { state: 'hidden', reason: null },
    frame: null,
    height: 28,
    padding: 12,
    detail: null,
    cpu: null,
    battery: null,
    wifi: null,
    media: null,
    weatherEpoch: '0',
    keepAwake: null,
    ...overrides,
  };
}

export function availableOutcome(
  overrides: Partial<BarView_Serialize> = {},
): Extract<BarSnapshotOutcome, { kind: 'available' }> {
  return { kind: 'available', view: barView(overrides) };
}

export function unavailableOutcome(
  sequence: string,
  reason: Extract<BarSnapshotOutcome, { kind: 'unavailable' }>['reason'] = 'disabled',
): Extract<BarSnapshotOutcome, { kind: 'unavailable' }> {
  return { kind: 'unavailable', reason, detail: null, generation: '1', sequence };
}
