#!/usr/bin/env bun
/**
 * Evidence-only collision guard for one owned PID of the measured identity
 * (`NEXT_BUNDLE_IDENTIFIER`, the release `com.marcosmoura.stache`).
 *
 * Reads Launch Services and the process table through the same query the
 * measurement tooling uses, prints the query and its verdict, and exits non-zero
 * when the PID is not the only next instance. It never signals or launches a
 * process. Part of the Teardown manual gates (`TEARDOWN-04-09`).
 */
import path from 'node:path';

import { DEFAULT_NEXT_APP, NEXT_BUNDLE_IDENTIFIER, REPOSITORY_ROOT } from './measure/paths';
import {
  InstanceCollisionError,
  assertNoOtherNextInstance,
  findRunningStacheProcesses,
} from './measure/process';
import type { Target } from './measure/types';

function bundleTarget(bundlePath: string): Target {
  const executablePath = path.join(bundlePath, 'Contents', 'MacOS', 'stache');

  return {
    kind: 'next',
    bundlePath,
    bundleIdentifier: NEXT_BUNDLE_IDENTIFIER,
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
const next = bundleTarget(nextBundle);
// The legacy app shared the release identifier and has been removed; nothing
// is classified as legacy here.
const query = (): ReturnType<typeof findRunningStacheProcesses> =>
  findRunningStacheProcesses(next, null, []);

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
