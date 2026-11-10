/**
 * Parses `top -l 2 -stats pid,idlew` output.
 *
 * The second (last) sample block holds the idle wakeups of the interval between
 * the two samples. A trailing `+` marks a rounded-up count and is dropped.
 */
export function parseTopWakeups(output: string): Map<number, number> {
  const lines = output.split('\n');
  let start = -1;

  for (const [index, line] of lines.entries()) {
    if (/^\s*PID\s+IDLEW\b/.test(line)) {
      start = index;
    }
  }

  const wakeups = new Map<number, number>();

  if (start < 0) {
    return wakeups;
  }

  for (const line of lines.slice(start + 1)) {
    const match = /^\s*(\d+)\s+(\d+)[+-]?\s*$/.exec(line);

    if (match !== null) {
      wakeups.set(Number.parseInt(match[1], 10), Number.parseInt(match[2], 10));
    }
  }

  return wakeups;
}

/** Wakeups per second summed over the tree's pids, or null when none was reported. */
export function treeWakeupsPerSecond(
  wakeups: Map<number, number>,
  pids: number[],
  intervalSeconds: number,
): number | null {
  if (!(intervalSeconds > 0)) {
    return null;
  }

  let total = 0;
  let seen = false;

  for (const pid of new Set(pids)) {
    const count = wakeups.get(pid);

    if (count !== undefined) {
      total += count;
      seen = true;
    }
  }

  return seen ? total / intervalSeconds : null;
}
