import { describe, expect, it } from 'vitest';

import { defaultMaxBusyCpuPercent, helpText, parseBarCommandLine, UsageError } from './options';

function run(args: string[]) {
  const parsed = parseBarCommandLine(args);

  if (parsed.kind !== 'run') {
    throw new Error('expected a run');
  }

  return parsed.options;
}

describe('parseBarCommandLine', () => {
  it('defaults to the documented minimums and the default series', () => {
    expect(run([])).toMatchObject({
      launchesPerArm: 5,
      warmupSeconds: 10,
      sampleSeconds: 60,
      height: 28,
      padding: 12,
      ssid: 'default',
    });
  });

  it('accepts larger values and the SSID-off series', () => {
    expect(
      run([
        '--app',
        '/x/Stache Next.app',
        '--launches',
        '7',
        '--warmup-seconds',
        '15',
        '--sample-seconds',
        '90',
        '--height',
        '30',
        '--padding',
        '0',
        '--ssid-off',
      ]),
    ).toMatchObject({
      appPath: '/x/Stache Next.app',
      launchesPerArm: 7,
      warmupSeconds: 15,
      sampleSeconds: 90,
      height: 30,
      padding: 0,
      ssid: 'off',
    });
  });

  it('scales the busy limit with the core count', () => {
    expect(defaultMaxBusyCpuPercent(10)).toBe(150);
    expect(defaultMaxBusyCpuPercent(8)).toBe(120);
    expect(defaultMaxBusyCpuPercent(1)).toBe(30);
    expect(defaultMaxBusyCpuPercent(0)).toBe(30);
    expect(parseBarCommandLine([])).toMatchObject({
      kind: 'run',
      options: { maxBusyCpuPercent: defaultMaxBusyCpuPercent() },
    });
    expect(parseBarCommandLine(['--max-busy-cpu', '40'])).toMatchObject({
      options: { maxBusyCpuPercent: 40 },
    });
  });

  it('refuses fewer than the documented minimums', () => {
    expect(() => parseBarCommandLine(['--launches', '4'])).toThrow(UsageError);
    expect(() => parseBarCommandLine(['--warmup-seconds', '9'])).toThrow(UsageError);
    expect(() => parseBarCommandLine(['--sample-seconds', '59'])).toThrow(UsageError);
  });

  it('refuses unknown flags, missing values and non-numbers', () => {
    expect(() => parseBarCommandLine(['--nope'])).toThrow(UsageError);
    expect(() => parseBarCommandLine(['--app'])).toThrow(UsageError);
    expect(() => parseBarCommandLine(['--app', '--launches'])).toThrow(UsageError);
    expect(() => parseBarCommandLine(['--launches', '5x'])).toThrow(UsageError);
  });

  it('prints help without validating anything else', () => {
    expect(parseBarCommandLine(['--help', '--launches', '1'])).toEqual({ kind: 'help' });
    expect(helpText()).toContain('Accessibility');
  });
});
