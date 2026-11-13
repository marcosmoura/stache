/** One entry of `gh run list --json databaseId,headSha,status,conclusion,createdAt,event`. */
export interface WorkflowRun {
  databaseId?: number;
  /** Only `push` runs count: a `pull_request` run tests a merge commit, not the tagged SHA. */
  event: string;
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
 * SHA must have completed successfully. Runs for other SHAs or from non-`push` events are ignored.
 */
export function qualityGate(runs: WorkflowRun[], sha: string): GateResult {
  const latest = runs
    .filter((run) => run.headSha === sha && run.event === 'push')
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
