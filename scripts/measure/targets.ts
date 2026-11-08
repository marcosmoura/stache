import { constants, accessSync, statSync } from 'node:fs';
import path from 'node:path';

import { REPOSITORY_ROOT } from './paths';
import { runCommand } from './process';
import type { BuildInfo, Target, TargetKind } from './types';

export class TargetError extends Error {}

/**
 * Resolves a release `.app` bundle or an executable into one measurable target.
 *
 * Relative inputs are resolved against the repository root, so the documented
 * command works from any working directory.
 */
export function resolveTarget(kind: TargetKind, inputPath: string): Target {
  const resolved = path.resolve(REPOSITORY_ROOT, inputPath);
  let stats: ReturnType<typeof statSync>;

  try {
    stats = statSync(resolved);
  } catch {
    throw new TargetError(
      `${kind} target does not exist: ${resolved}\n` +
        (kind === 'next'
          ? 'Build it with `bun run tauri:build` or pass --new-app <path>.'
          : 'Build the legacy release bundle or pass --legacy-app <path>.'),
    );
  }

  if (stats.isDirectory()) {
    if (!resolved.endsWith('.app')) {
      throw new TargetError(`${kind} directory is not a macOS .app bundle: ${resolved}`);
    }

    return resolveBundle(kind, resolved);
  }

  if (!stats.isFile()) {
    throw new TargetError(`${kind} target is not a file or .app bundle: ${resolved}`);
  }

  assertExecutable(resolved, kind);
  assertReleaseBuild(resolved, kind);

  return {
    kind,
    bundlePath: null,
    bundleIdentifier: null,
    executablePath: resolved,
    build: buildInfo(resolved, null),
  };
}

function resolveBundle(kind: TargetKind, bundlePath: string): Target {
  const executableName = readBundleExecutable(bundlePath);
  const executablePath = path.join(bundlePath, 'Contents', 'MacOS', executableName);

  assertExecutable(executablePath, kind);
  assertReleaseBuild(executablePath, kind);

  return {
    kind,
    bundlePath,
    bundleIdentifier: readBundleIdentifier(bundlePath),
    executablePath,
    build: buildInfo(executablePath, bundlePath),
  };
}

function buildInfo(executablePath: string, bundlePath: string | null): BuildInfo {
  const stats = statSync(executablePath);
  const releasePath = `${executablePath}${bundlePath ?? ''}`.toLowerCase().includes('/release/');

  return {
    executablePath,
    bundlePath,
    sizeBytes: stats.size,
    modifiedAt: stats.mtime.toISOString(),
    pathIncludesReleaseDirectory: releasePath,
  };
}

function readBundleExecutable(bundlePath: string): string {
  const infoPlist = path.join(bundlePath, 'Contents', 'Info.plist');

  try {
    const executable = runCommand('/usr/libexec/PlistBuddy', [
      '-c',
      'Print :CFBundleExecutable',
      infoPlist,
    ]).trim();

    if (executable.length === 0) {
      throw new Error('empty CFBundleExecutable');
    }

    return executable;
  } catch (error) {
    throw new TargetError(
      `cannot read CFBundleExecutable from ${infoPlist}: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
}

function readBundleIdentifier(bundlePath: string): string | null {
  const infoPlist = path.join(bundlePath, 'Contents', 'Info.plist');

  try {
    const identifier = runCommand('/usr/libexec/PlistBuddy', [
      '-c',
      'Print :CFBundleIdentifier',
      infoPlist,
    ]).trim();

    return identifier.length === 0 ? null : identifier;
  } catch {
    return null;
  }
}

function assertExecutable(executablePath: string, kind: TargetKind): void {
  try {
    accessSync(executablePath, constants.X_OK);
  } catch {
    throw new TargetError(`${kind} executable is missing or not executable: ${executablePath}`);
  }
}

function assertReleaseBuild(executablePath: string, kind: TargetKind): void {
  if (executablePath.toLowerCase().includes('/debug/')) {
    throw new TargetError(
      `${kind} target is a debug build: ${executablePath}. ` +
        'Measure release binaries only; build the release bundle first.',
    );
  }
}
