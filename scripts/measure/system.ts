import { fileURLToPath } from 'node:url';

import { REPOSITORY_ROOT } from './paths';
import { CommandExecutionError, commandExists, runCommand } from './process';
import type {
  AudioDeviceInfo,
  AudioState,
  DisplayInfo,
  EnvironmentSnapshot,
  MachineInfo,
  OsInfo,
  PowerInfo,
  RepositoryInfo,
  ToolchainInfo,
} from './types';

/** Validated view of `system_profiler SPAudioDataType -json`. */
export function readAudioState(): AudioState {
  return parseAudioState(runCommand('/usr/sbin/system_profiler', ['SPAudioDataType', '-json']));
}

export function parseAudioState(output: string): AudioState {
  const document = parseJson(output, 'system_profiler SPAudioDataType');
  const groups = asArray(asRecord(document)?.['SPAudioDataType']) ?? [];
  const devices: AudioDeviceInfo[] = [];

  for (const group of groups) {
    const items = asArray(asRecord(group)?.['_items']) ?? [];

    for (const item of items) {
      const device = parseAudioDevice(item);

      if (device !== null) {
        devices.push(device);
      }
    }
  }

  return {
    devices,
    defaultInput: devices.find((device) => device.defaultInput)?.name ?? null,
    defaultOutput: devices.find((device) => device.defaultOutput)?.name ?? null,
  };
}

function parseAudioDevice(value: unknown): AudioDeviceInfo | null {
  const item = asRecord(value);

  if (item === null) {
    return null;
  }

  const name = asString(item['_name']);

  if (name === null) {
    return null;
  }

  const transport = asString(item['coreaudio_device_transport']);
  const lowerName = name.toLowerCase();

  return {
    name,
    manufacturer: asString(item['coreaudio_device_manufacturer']),
    transport,
    inputChannels: asNumber(item['coreaudio_device_input']),
    outputChannels: asNumber(item['coreaudio_device_output']),
    hdmi: (transport?.includes('hdmi') ?? false) || lowerName.includes('hdmi'),
    builtIn:
      lowerName.includes('macbook') ||
      lowerName.includes('built-in') ||
      lowerName.includes('builtin') ||
      (transport?.includes('builtin') ?? false),
    airPlay: (transport?.includes('airplay') ?? false) || lowerName.includes('airplay'),
    defaultInput: item['coreaudio_default_audio_input_device'] === 'spaudio_yes',
    defaultOutput: item['coreaudio_default_audio_output_device'] === 'spaudio_yes',
  };
}

/** Validated view of `system_profiler SPDisplaysDataType -json`. */
export function parseDisplays(output: string): DisplayInfo[] {
  const document = parseJson(output, 'system_profiler SPDisplaysDataType');
  const groups = asArray(asRecord(document)?.['SPDisplaysDataType']) ?? [];
  const displays: DisplayInfo[] = [];

  for (const group of groups) {
    const groupRecord = asRecord(group);

    if (groupRecord === null) {
      continue;
    }

    const gpu = asString(groupRecord['sppci_model']) ?? asString(groupRecord['_name']);

    for (const display of asArray(groupRecord['spdisplays_ndrvs']) ?? []) {
      const item = asRecord(display);

      if (item === null) {
        continue;
      }

      displays.push({
        gpu,
        name: asString(item['_name']),
        resolution: asString(item['_spdisplays_resolution']),
        main: item['spdisplays_main'] === 'spdisplays_yes',
        mirroring: item['spdisplays_mirror'] === 'spdisplays_on',
        online: item['spdisplays_online'] === 'spdisplays_yes',
      });
    }
  }

  return displays;
}

/** Validated view of `pmset -g batt`. */
export function parsePower(output: string): PowerInfo {
  const source = /Now drawing from '([^']+)'/.exec(output)?.[1] ?? 'unknown';
  const batteryPercent = /(\d+)%/.exec(output)?.[1];

  return {
    source,
    batteryPercent: batteryPercent === undefined ? null : Number.parseInt(batteryPercent, 10),
    raw: output.trim(),
  };
}

export function collectEnvironment(): EnvironmentSnapshot {
  return {
    machine: collectMachine(),
    os: collectOperatingSystem(),
    toolchain: collectToolchain(),
    repository: collectRepository(),
    power: parsePower(runCommand('/usr/bin/pmset', ['-g', 'batt'])),
    displays: parseDisplays(
      runCommand('/usr/sbin/system_profiler', ['SPDisplaysDataType', '-json']),
    ),
    audio: readAudioState(),
  };
}

function collectMachine(): MachineInfo {
  const model = runCommand('/usr/sbin/sysctl', ['-n', 'hw.model']).trim();
  const chip = runCommand('/usr/sbin/sysctl', ['-n', 'machdep.cpu.brand_string']).trim();
  const memory = runCommand('/usr/sbin/sysctl', ['-n', 'hw.memsize']).trim();

  return {
    model,
    chip,
    memoryBytes: Number.parseInt(memory, 10),
  };
}

function collectOperatingSystem(): OsInfo {
  return {
    productVersion: runCommand('/usr/bin/sw_vers', ['-productVersion']).trim(),
    buildVersion: runCommand('/usr/bin/sw_vers', ['-buildVersion']).trim(),
  };
}

function collectToolchain(): ToolchainInfo {
  return {
    rustc: toolVersion('rustc', ['--version']),
    cargo: toolVersion('cargo', ['--version']),
  };
}

function collectRepository(): RepositoryInfo {
  const commit = runCommand('/usr/bin/git', ['-C', REPOSITORY_ROOT, 'rev-parse', 'HEAD']).trim();
  const branch = runCommand('/usr/bin/git', [
    '-C',
    REPOSITORY_ROOT,
    'rev-parse',
    '--abbrev-ref',
    'HEAD',
  ]).trim();
  const status = runCommand('/usr/bin/git', [
    '-C',
    REPOSITORY_ROOT,
    'status',
    '--porcelain',
  ]).trim();

  return { root: REPOSITORY_ROOT, commit, branch, dirty: status.length > 0 };
}

function toolVersion(command: string, args: string[]): string {
  try {
    return runCommand(command, args).trim();
  } catch {
    return 'unavailable';
  }
}

/** One best-effort attempt to put a default device back after contamination. */
export interface AudioRestoreResult {
  attempted: boolean;
  succeeded: boolean;
  detail: string;
}

export function restoreDefaultDevice(
  direction: 'input' | 'output',
  name: string,
): AudioRestoreResult {
  try {
    if (commandExists('SwitchAudioSource')) {
      runCommand('SwitchAudioSource', ['-t', direction, '-s', name]);

      return {
        attempted: true,
        succeeded: true,
        detail: `SwitchAudioSource restored the default ${direction} to "${name}"`,
      };
    }

    if (commandExists('swift')) {
      const helper = fileURLToPath(
        new URL('./restore-default-audio-device.swift', import.meta.url),
      );

      runCommand('/usr/bin/swift', [helper, direction, name]);

      return {
        attempted: true,
        succeeded: true,
        detail: `restore-default-audio-device.swift restored the default ${direction} to "${name}"`,
      };
    }

    return {
      attempted: false,
      succeeded: false,
      detail:
        `no audio writer is available; set the default ${direction} back to "${name}" ` +
        'in System Settings > Sound (or install SwitchAudioSource)',
    };
  } catch (error) {
    const detail = error instanceof CommandExecutionError ? error.message : String(error);

    return { attempted: true, succeeded: false, detail };
  }
}

function parseJson(output: string, label: string): unknown {
  try {
    return JSON.parse(output) as unknown;
  } catch (error) {
    throw new Error(`${label} did not return JSON: ${String(error)}`);
  }
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return null;
  }

  return value as Record<string, unknown>;
}

function asArray(value: unknown): unknown[] | null {
  return Array.isArray(value) ? value : null;
}

function asString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

function asNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}
