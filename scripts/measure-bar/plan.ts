import type { Arm } from './types';

/**
 * The launch order: pair 0 runs off then on, pair 1 on then off, and so on, so
 * slow drift (thermals, background work) lands on both arms equally.
 */
export function armOrder(launchesPerArm: number): Arm[] {
  const order: Arm[] = [];

  for (let pair = 0; pair < launchesPerArm; pair += 1) {
    order.push(...(pair % 2 === 0 ? (['off', 'on'] as const) : (['on', 'off'] as const)));
  }

  return order;
}

/** The pair a schedule position belongs to. */
export function pairOf(index: number): number {
  return Math.floor(index / 2);
}

/** The per-trial `bar.toml`: only `enabled` differs between the arms. */
export function renderBarToml(arm: Arm, height: number, padding: number): string {
  return [
    `enabled = ${arm === 'on' ? 'true' : 'false'}`,
    `height = ${height}`,
    `padding = ${padding}`,
    '',
  ].join('\n');
}

/** The per-trial `global.toml`: no startup commands, quiet logging. */
export function renderGlobalToml(): string {
  return ['exec_on_startup = []', '', '[logging]', 'level = "info"', 'debug = false', ''].join(
    '\n',
  );
}

/** Environment variables a series adds to the child (nothing for the default series). */
export function seriesEnvironment(ssid: 'default' | 'off'): Record<string, string> {
  return ssid === 'off' ? { STACHE_BAR_WIFI_SSID_SUMMARY: '0' } : {};
}

export function seriesLabel(ssid: 'default' | 'off'): string {
  return ssid === 'off' ? 'SSID off (STACHE_BAR_WIFI_SSID_SUMMARY=0)' : 'default (SSID path on)';
}
