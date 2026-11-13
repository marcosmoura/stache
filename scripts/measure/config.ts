import { mkdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { NEXT_BUNDLE_IDENTIFIER } from './paths';
import { runCommand } from './process';
import type { AudioDeviceInfo, AudioState, ConfigFile, DisplayInfo } from './types';

const SOCKET_SUFFIX = `/${NEXT_BUNDLE_IDENTIFIER}/ipc`;
const MAXIMUM_SOCKET_PATH_BYTES = 103;
const TEMP_DIRECTORY_NAME_LENGTH = 4;

export interface ScenarioConfig {
  directory: string;
  files: ConfigFile[];
}

/**
 * Creates one short temporary root under the approved opencode temp directory.
 *
 * `HOME`, `TMPDIR` and the runtime socket all derive from this path, so the
 * resolved socket is checked against the 103-byte macOS `sun_path` limit before
 * anything is launched.
 */
export function createTempRoot(): string {
  const base = path.join(os.tmpdir(), 'opencode');

  mkdirSync(base, { recursive: true });

  const root = runCommand('/usr/bin/mktemp', ['-d', path.join(base, 'XXXX')]).trim();
  const canonical = realpathSync(root);
  const socketLength = Math.max(root.length, canonical.length) + SOCKET_SUFFIX.length;

  if (socketLength > MAXIMUM_SOCKET_PATH_BYTES) {
    removeTempRoot(canonical);
    throw new Error(
      `temporary root would produce a ${socketLength}-byte socket path ` +
        `(limit ${MAXIMUM_SOCKET_PATH_BYTES}): ${canonical}`,
    );
  }

  return canonical;
}

/** Removes a temporary root, refusing anything this tool did not create. */
export function removeTempRoot(root: string): void {
  const name = path.basename(root);
  const parent = path.basename(path.dirname(root));

  if (name.length !== TEMP_DIRECTORY_NAME_LENGTH || parent !== 'opencode') {
    throw new Error(`refusing to remove a directory this tool did not create: ${root}`);
  }

  rmSync(root, { recursive: true, force: true });
}

/** Writes the two TOML configuration files used by a Stache scenario. */
export function writeNextConfigs(
  root: string,
  enabled: boolean,
  audio: AudioState,
): ScenarioConfig {
  const directory = path.join(root, 'config', 'stache');

  mkdirSync(directory, { recursive: true });

  const globalContent = [
    'exec_on_startup = []',
    '',
    '[logging]',
    'level = "info"',
    'debug = false',
    '',
  ].join('\n');
  const proxyAudioContent = [
    `enabled = ${enabled ? 'true' : 'false'}`,
    ...priorityTables('input', enabled ? priorityList(audio.defaultInput) : []),
    ...priorityTables('output', enabled ? priorityList(audio.defaultOutput) : []),
    '',
  ].join('\n');

  const files = [
    writeConfigFile(directory, 'global.toml', globalContent),
    writeConfigFile(directory, 'proxyAudio.toml', proxyAudioContent),
  ];

  return { directory, files };
}

/** Renders one priority list as `input = []` or an array of tables. */
function priorityTables(key: 'input' | 'output', entries: PriorityEntry[]): string[] {
  if (entries.length === 0) {
    return [`${key} = []`];
  }

  return entries.flatMap((entry) => [
    '',
    `[[${key}]]`,
    `name = ${tomlString(entry.name)}`,
    `strategy = ${tomlString(entry.strategy)}`,
  ]);
}

/** A TOML basic string: JSON escaping plus the DEL control character. */
function tomlString(value: string): string {
  return JSON.stringify(value).replaceAll('\u007f', '\\u007f');
}

/**
 * Writes an explicit legacy config that disables every configurable tool.
 *
 * The legacy tray, bar and widget windows are unconditional and stay as
 * recorded baseline overhead; the report never claims they were disabled.
 */
export function writeLegacyConfig(root: string): ScenarioConfig {
  const directory = path.join(root, 'config', 'stache');

  mkdirSync(directory, { recursive: true });
  const content = `${JSON.stringify(
    {
      bar: { enabled: false },
      commandQuit: { enabled: false },
      wallpapers: { enabled: false },
      keybindings: {},
      execOnStartup: [],
      menuAnywhere: { enabled: false },
      proxyAudio: { enabled: false },
      notunes: { enabled: false },
      tiling: { enabled: false },
    },
    null,
    2,
  )}\n`;

  return {
    directory,
    files: [writeConfigFile(directory, 'config.jsonc', content)],
  };
}

function writeConfigFile(directory: string, fileName: string, content: string): ConfigFile {
  const filePath = path.join(directory, fileName);

  writeFileSync(filePath, content, 'utf8');

  return { path: filePath, content };
}

interface PriorityEntry {
  name: string;
  strategy: 'exact';
}

function priorityList(name: string | null): PriorityEntry[] {
  return name === null ? [] : [{ name, strategy: 'exact' }];
}

/**
 * Refuses an enabled pilot that could change the default device before its
 * configured priorities run.
 *
 * Selection order puts mirroring AirPlay, a current AirPlay device and HDMI
 * above the configured list. When one of those rules would pick a different
 * device, no priority configuration can make the pilot inert.
 */
export function assertPilotIsInert(audio: AudioState, displays: DisplayInfo[]): void {
  const defaultOutput = findDevice(audio, audio.defaultOutput);
  const defaultInput = findDevice(audio, audio.defaultInput);

  if (audio.defaultOutput !== null && defaultOutput === null) {
    throw new Error(
      `cannot locate the default output "${audio.defaultOutput}" in the device list; ` +
        'refusing to enable proxyAudio because its priorities cannot be proven inert',
    );
  }

  if (audio.defaultInput !== null && defaultInput === null) {
    throw new Error(
      `cannot locate the default input "${audio.defaultInput}" in the device list; ` +
        'refusing to enable proxyAudio because its priorities cannot be proven inert',
    );
  }

  if (defaultOutput?.builtIn === true) {
    const hdmi = audio.devices.find((device) => device.hdmi && (device.outputChannels ?? 0) > 0);

    if (hdmi !== undefined) {
      throw new Error(
        'proxyAudio cannot be measured inertly while the default output is built in and ' +
          `an HDMI output (${hdmi.name}) is connected: the HDMI rule runs before priorities. ` +
          'Disconnect the HDMI output and rerun.',
      );
    }
  }

  if (displays.some((display) => display.mirroring)) {
    assertNoMirroringAirPlayOverride(audio, 'output', defaultOutput);
    assertNoMirroringAirPlayOverride(audio, 'input', defaultInput);
  }
}

function assertNoMirroringAirPlayOverride(
  audio: AudioState,
  direction: 'input' | 'output',
  current: AudioDeviceInfo | null,
): void {
  if (current === null) {
    return;
  }

  const capable = audio.devices.filter((device) => {
    if (!device.airPlay) {
      return false;
    }

    return direction === 'input'
      ? (device.inputChannels ?? 0) > 0
      : (device.outputChannels ?? 0) > 0;
  });
  const first = capable[0];

  if (first !== undefined && first.name !== current?.name) {
    throw new Error(
      `screen mirroring is active and the pilot would choose AirPlay ` +
        `${direction} "${first.name}" before priorities. Disable mirroring and rerun.`,
    );
  }
}

function findDevice(audio: AudioState, name: string | null): AudioDeviceInfo | null {
  if (name === null) {
    return null;
  }

  return audio.devices.find((device) => device.name === name) ?? null;
}
