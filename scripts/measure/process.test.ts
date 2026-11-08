import { describe, expect, it } from 'vitest';

import { InstanceCollisionError, assertNoOtherNextInstance } from './process';
import type { RunningStacheProcess } from './process';

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
