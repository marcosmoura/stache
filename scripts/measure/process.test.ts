import { describe, expect, it } from 'vitest';

import { DEVELOPMENT_BUNDLE_IDENTIFIER, NEXT_BUNDLE_IDENTIFIER } from './paths';
import {
  InstanceCollisionError,
  assertNoOtherNextInstance,
  classifyApplication,
  classifyExecutable,
} from './process';
import type { RunningStacheProcess } from './process';
import type { Target } from './types';

function target(kind: 'next' | 'legacy', bundlePath: string): Target {
  const executablePath = `${bundlePath}/Contents/MacOS/stache`;

  return {
    kind,
    bundlePath,
    bundleIdentifier: NEXT_BUNDLE_IDENTIFIER,
    executablePath,
    build: {
      executablePath,
      bundlePath,
      sizeBytes: 0,
      modifiedAt: new Date(0).toISOString(),
      pathIncludesReleaseDirectory: true,
    },
  };
}

const MEASURED = target('next', '/repo/target/release/bundle/macos/Stache.app');
const INSTALLED = '/Applications/Stache.app/Contents/MacOS/stache';

function entry(kind: 'next' | 'legacy', pid: number | null): RunningStacheProcess {
  return { kind, pid, source: 'lsappinfo', description: `${kind} pid ${pid ?? 'unknown'}` };
}

function refusal(query: () => RunningStacheProcess[], ownedPid: number): InstanceCollisionError {
  try {
    assertNoOtherNextInstance(query, ownedPid);
  } catch (error) {
    if (error instanceof InstanceCollisionError) {
      return error;
    }

    throw error;
  }

  throw new Error('expected the guard to refuse');
}

describe('assertNoOtherNextInstance', () => {
  it('accepts the owned next instance and returns the query as evidence', () => {
    const evidence = [entry('next', 42), entry('legacy', 7)];

    expect(assertNoOtherNextInstance(() => evidence, 42)).toEqual(evidence);
  });

  it('refuses a foreign next instance', () => {
    const evidence = [entry('next', 42), entry('next', 99)];
    const error = refusal(() => evidence, 42);

    expect(error.reason).toBe('foreign-instance');
    expect(error.evidence).toEqual(evidence);
    expect(error.message).toContain('99');
  });

  it('refuses a next instance whose identity cannot be verified', () => {
    const evidence = [entry('next', 42), entry('next', null)];

    expect(refusal(() => evidence, 42).reason).toBe('unknown-identity');
  });

  it('does not treat a legacy process as the owned next instance', () => {
    const evidence = [entry('legacy', 42)];
    const error = refusal(() => evidence, 42);

    expect(error.reason).toBe('unknown-identity');
    expect(error.evidence).toEqual(evidence);
  });

  it('refuses when no next instance is reported at all', () => {
    expect(refusal(() => [], 42).reason).toBe('unknown-identity');
  });

  it('refuses a failed query instead of assuming nothing is running', () => {
    const error = refusal(() => {
      throw new Error('lsappinfo is unavailable');
    }, 42);

    expect(error.reason).toBe('query-failed');
    expect(error.evidence).toEqual([]);
    expect(error.message).toContain('lsappinfo is unavailable');
  });
});

describe('process classification', () => {
  it('measures the release identity', () => {
    expect(NEXT_BUNDLE_IDENTIFIER).toBe('com.marcosmoura.stache');
  });

  it('never classifies the new app as legacy without an explicit --legacy-app', () => {
    expect(classifyExecutable(MEASURED.executablePath, MEASURED, null)).toBe('next');
    expect(classifyExecutable(INSTALLED, MEASURED, null)).toBe('next');
    expect(
      classifyApplication(
        {
          name: 'Stache',
          bundleIdentifier: NEXT_BUNDLE_IDENTIFIER,
          executablePath: INSTALLED,
          pid: 7,
        },
        null,
      ),
    ).toBe('next');
  });

  it('classifies legacy only from the explicit --legacy-app path', () => {
    const legacy = target('legacy', '/Volumes/Old/Stache.app');

    expect(classifyExecutable(legacy.executablePath, MEASURED, legacy)).toBe('legacy');
    expect(classifyExecutable(INSTALLED, MEASURED, legacy)).toBe('next');
    expect(
      classifyApplication(
        {
          name: 'Stache',
          bundleIdentifier: NEXT_BUNDLE_IDENTIFIER,
          executablePath: legacy.executablePath,
          pid: 8,
        },
        legacy,
      ),
    ).toBe('legacy');
  });

  it('blocks on a bundled development app too', () => {
    expect(
      classifyApplication(
        {
          name: 'Stache Next',
          bundleIdentifier: DEVELOPMENT_BUNDLE_IDENTIFIER,
          executablePath: '/Users/me/Stache Next.app/Contents/MacOS/stache',
          pid: 10,
        },
        null,
      ),
    ).toBe('next');
    expect(
      classifyExecutable('/Users/me/Stache Next.app/Contents/MacOS/stache', MEASURED, null),
    ).toBe('next');
  });

  it('ignores other identifiers and unrelated executables', () => {
    const app = { name: 'Dev', executablePath: null, pid: 9 };

    expect(
      classifyApplication({ ...app, bundleIdentifier: 'com.marcosmoura.stache.helper' }, null),
    ).toBeNull();
    expect(classifyExecutable('/usr/local/bin/stache', MEASURED, null)).toBeNull();
  });
});
