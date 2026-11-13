import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { readWorkspaceVersion } from './pipeline';

/** One entry of `gh run list --json databaseId,headSha,status,conclusion,createdAt`. */
export interface WorkflowRun {
  databaseId?: number;
  headSha: string;
  status: string;
  conclusion: string | null;
  createdAt: string;
}

export type GateResult =
  | { state: 'pass'; runId?: number }
  | { state: 'pending' | 'blocked'; reason: string };

/** Returns a problem when the tag is not `v<version>` or `v<version>-rc.<n>`. */
export function tagVersionProblem(tag: string, version: string): string | null {
  const match = /^v(\d+\.\d+\.\d+)(?:-rc\.(\d+))?$/.exec(tag);

  if (!match) {
    return `tag ${tag} is not v<version> or v<version>-rc.<n>`;
  }

  return match[1] === version ? null : `tag ${tag} does not match version ${version}`;
}

/**
 * Decides from Quality runs whether the exact SHA may be signed: the newest Quality run for that
 * SHA must have completed successfully. Runs for other SHAs are ignored.
 */
export function qualityGate(runs: WorkflowRun[], sha: string): GateResult {
  const latest = runs
    .filter((run) => run.headSha === sha)
    .toSorted((a, b) => b.createdAt.localeCompare(a.createdAt))[0];

  if (!latest) {
    return { state: 'blocked', reason: `no Quality run for ${sha}` };
  }

  if (latest.status !== 'completed') {
    return { state: 'pending', reason: `Quality run for ${sha} is ${latest.status}` };
  }

  if (latest.conclusion !== 'success') {
    return { state: 'blocked', reason: `Quality run for ${sha} concluded ${latest.conclusion}` };
  }

  return { state: 'pass', runId: latest.databaseId };
}

// CLI: bun scripts/release/gate.ts <tag> <sha> <runs.json>
// Exit 0 pass, 2 pending, 1 blocked or tag/version mismatch.
if (import.meta.main) {
  const [tag, sha, runsPath] = process.argv.slice(2);
  const version = readWorkspaceVersion(
    readFileSync(join(import.meta.dirname, '../../Cargo.toml'), 'utf8'),
  );
  const tagProblem = tagVersionProblem(tag ?? '', version);

  if (tagProblem) {
    console.error(tagProblem);
    process.exit(1);
  }

  const result = qualityGate(JSON.parse(readFileSync(runsPath, 'utf8')) as WorkflowRun[], sha);

  if (result.state === 'pass') {
    console.log(`Quality passed for ${sha} (run ${result.runId ?? 'unknown'}).`);
  } else {
    console.error(result.reason);
    process.exit(result.state === 'pending' ? 2 : 1);
  }
}
