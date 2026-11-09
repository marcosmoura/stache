import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { afterEach, describe, expect, it } from 'vitest';

import { writeLegacyConfig, writeNextConfigs } from './config';
import type { AudioState } from './types';

const audio: AudioState = {
  devices: [],
  defaultInput: 'Built-in "Mic"',
  defaultOutput: 'Speakers',
};

const roots: string[] = [];

function tempRoot(): string {
  const root = mkdtempSync(path.join(os.tmpdir(), 'stache-config-test-'));

  roots.push(root);

  return root;
}

afterEach(() => {
  for (const root of roots.splice(0)) {
    rmSync(root, { recursive: true, force: true });
  }
});

describe('writeNextConfigs', () => {
  it('writes TOML files named after the catalog ids', () => {
    const { directory, files } = writeNextConfigs(tempRoot(), true, audio);

    expect(files.map((file) => path.basename(file.path))).toEqual([
      'global.toml',
      'proxyAudio.toml',
    ]);
    expect(readFileSync(path.join(directory, 'global.toml'), 'utf8')).toBe(
      'exec_on_startup = []\n\n[logging]\nlevel = "info"\ndebug = false\n',
    );
  });

  it('writes an enabled proxyAudio file with quoted priority tables', () => {
    const { files } = writeNextConfigs(tempRoot(), true, audio);

    expect(files[1]?.content).toBe(
      [
        'enabled = true',
        '',
        '[[input]]',
        'name = "Built-in \\"Mic\\""',
        'strategy = "exact"',
        '',
        '[[output]]',
        'name = "Speakers"',
        'strategy = "exact"',
        '',
      ].join('\n'),
    );
  });

  it('writes a disabled proxyAudio file with empty lists', () => {
    const { files } = writeNextConfigs(tempRoot(), false, audio);

    expect(files[1]?.content).toBe('enabled = false\ninput = []\noutput = []\n');
  });

  it('never writes JSON syntax into a runtime file', () => {
    const { files } = writeNextConfigs(tempRoot(), true, audio);

    for (const file of files) {
      expect(file.content.trimStart().startsWith('{')).toBe(false);
    }
  });
});

describe('writeLegacyConfig', () => {
  it('keeps the legacy single JSONC file for the legacy application', () => {
    const { files } = writeLegacyConfig(tempRoot());

    expect(files.map((file) => path.basename(file.path))).toEqual(['config.jsonc']);
    expect(JSON.parse(files[0]?.content ?? '')).toMatchObject({ execOnStartup: [] });
  });
});
