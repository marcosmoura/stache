import { spawnSync } from 'node:child_process';
import { existsSync, statSync } from 'node:fs';

if (process.env.HUSKY === '0') {
  console.log('HUSKY=0: skipping hook installation.');
} else if (existsSync('.git') && statSync('.git').isFile()) {
  console.log('Linked worktree: skipping Husky installation to preserve shared Git config.');
  console.log(
    'If local shims are absent, add node_modules/.bin to PATH before invoking hooks with sh.',
  );
} else {
  const result = spawnSync('husky', [], {
    stdio: 'inherit',
  });
  if (result.error) throw result.error;
  process.exit(result.status ?? 1);
}
