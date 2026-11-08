#!/usr/bin/env bun
/**
 * Evidence-only collision guard for one owned `com.marcosmoura.stache.next` PID.
 *
 * Reads Launch Services and the process table through the same query the
 * measurement tooling uses, prints the query and its verdict, and exits non-zero
 * when the PID is not the only next instance. It never signals or launches a
 * process. Part of the Teardown manual gates (`TEARDOWN-04-09`).
 */
import { existsSync } from 'node:fs';
import path from 'node:path';

import {
  DEFAULT_NEXT_APP,
  LEGACY_BUNDLE_IDENTIFIER,
  NEXT_BUNDLE_IDENTIFIER,
  REPOSITORY_ROOT,
} from './measure/paths';
import {
  InstanceCollisionError,
  assertNoOtherNextInstance,
  findRunningStacheProcesses,
} from './measure/process';
import type { Target } from './measure/types';

function bundleTarget(kind: 'next' | 'legacy', bundlePath: string): Target {
  const executablePath = path.join(bundlePath, 'Contents', 'MacOS', 'stache');

  return {
    kind,
    bundlePath,
    bundleIdentifier: kind === 'next' ? NEXT_BUNDLE_IDENTIFIER : LEGACY_BUNDLE_IDENTIFIER,
    executablePath,
    build: {
      executablePath,
      bundlePath,
      sizeBytes: 0,
      modifiedAt: new Date(0).toISOString(),
      pathIncludesReleaseDirectory: false,
    },
  };
}

const ownedPid = Number.parseInt(process.argv[2] ?? '', 10);

if (!Number.isInteger(ownedPid) || ownedPid <= 0) {
  console.error('usage: bun scripts/check-next-instance.ts <owned-pid>');
  process.exit(2);
}

const nextBundle = path.resolve(REPOSITORY_ROOT, process.env.STACHE_NEXT_APP ?? DEFAULT_NEXT_APP);
const legacyBundle = '/Applications/Stache.app';
const next = bundleTarget('next', nextBundle);
const legacy = existsSync(legacyBundle) ? bundleTarget('legacy', legacyBundle) : null;
const query = (): ReturnType<typeof findRunningStacheProcesses> =>
  findRunningStacheProcesses(next, legacy, []);

try {
  const evidence = assertNoOtherNextInstance(query, ownedPid);

  console.log(`query: ${JSON.stringify(evidence)}`);
  console.log(`result: ${JSON.stringify({ ok: true, ownedPid })}`);
} catch (error) {
  if (error instanceof InstanceCollisionError) {
    console.log(`query: ${JSON.stringify(error.evidence)}`);
    console.log(
      `result: ${JSON.stringify({
        ok: false,
        ownedPid,
        reason: error.reason,
        message: error.message,
      })}`,
    );
    process.exit(1);
  }

  throw error;
}
