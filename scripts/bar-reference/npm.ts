import { execFileSync } from 'node:child_process';
/**
 * Downloads and verifies the exact legacy icon packages (manual extraction step only).
 *
 * Versions and integrity hashes come from the pinned legacy `pnpm-lock.yaml`. Tarballs are
 * extracted into a fresh temporary directory; only the SVG data files the Bar uses are read, and
 * the package code that renders them (`HugeiconsIcon`, `SiTidal`) is evaluated against the
 * repository's own React.
 */
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import * as React from 'react';
import * as jsxRuntime from 'react/jsx-runtime';

import { evaluateCommonJs } from './loader.ts';

export interface PackageSpec {
  name: string;
  version: string;
}

export const LEGACY_ICON_PACKAGES: readonly PackageSpec[] = [
  { name: '@hugeicons/core-free-icons', version: '4.2.3' },
  { name: '@hugeicons/react', version: '1.1.9' },
  { name: '@icons-pack/react-simple-icons', version: '13.15.1' },
];

/**
 * The strip's hugeicons glyphs come from the repository version (user decision 2026-11-10), not the
 * legacy 4.2.3. Its integrity is read from the repository `bun.lock`.
 */
export const STRIP_HUGEICONS_PACKAGE: PackageSpec = {
  name: '@hugeicons/core-free-icons',
  version: '4.4.0',
};

/** Integrity of `spec` in a Bun text lockfile (`"name": ["name@version", "", {...}, "sha512-..."]`). */
export function bunLockIntegrity(lockfile: string, spec: PackageSpec): string {
  const escaped = `${spec.name}@${spec.version}`.replace(/[/@.]/g, '\\$&');
  const match = new RegExp(
    `^ {4}"[^"]+": \\["${escaped}", [^\\n]*"(sha512-[^"]+)"\\],?$`,
    'm',
  ).exec(lockfile);

  if (!match) {
    throw new Error(`${spec.name}@${spec.version} is not in the repository bun.lock`);
  }

  return match[1];
}

export interface PackageRecord extends PackageSpec {
  integrity: string;
  registryUrl: string;
  license: string;
  licenseFile: { path: string; sha256: string } | null;
}

export function lockIntegrity(lockfile: string, spec: PackageSpec): string {
  const escaped = `${spec.name}@${spec.version}`.replace(/[/@.]/g, '\\$&');
  const match = new RegExp(
    `^ {2}'${escaped}':\\n {4}resolution: \\{integrity: (sha512-[^,}]+)`,
    'm',
  ).exec(lockfile);

  if (!match) {
    throw new Error(`${spec.name}@${spec.version} is not in the legacy lockfile`);
  }

  return match[1];
}

function sha256(buffer: Buffer | string): string {
  return createHash('sha256').update(buffer).digest('hex');
}

export function aliasTarget(bundle: string, exportName: string): string {
  const match = new RegExp(`^exports\\.${exportName} = (\\w+);$`, 'm').exec(bundle);

  if (!match) {
    throw new Error(`No export statement for ${exportName}`);
  }

  return match[1];
}

export class IconPackages {
  readonly records: PackageRecord[] = [];
  /** Imported `@hugeicons/core-free-icons` export name -> definition constant it aliases. */
  readonly usedHugeicons = new Map<string, string>();
  readonly usedFiles: { packageName: string; file: string; sha256: string }[] = [];
  private readonly directories = new Map<string, string>();
  private readonly workspace = mkdtempSync(path.join(os.tmpdir(), 'bar-reference-'));
  private reactModule: Record<string, unknown> | null = null;

  async download(spec: PackageSpec, integrity: string): Promise<void> {
    const base = spec.name.split('/').pop();
    const registryUrl = `https://registry.npmjs.org/${spec.name}/-/${base}-${spec.version}.tgz`;
    const response = await fetch(registryUrl);

    if (!response.ok) {
      throw new Error(`Download failed (${response.status}): ${registryUrl}`);
    }

    const tarball = Buffer.from(await response.arrayBuffer());
    const actual = `sha512-${createHash('sha512').update(tarball).digest('base64')}`;

    if (actual !== integrity) {
      throw new Error(
        `Integrity mismatch for ${spec.name}@${spec.version}: ${actual} != ${integrity}`,
      );
    }

    const directory = mkdtempSync(path.join(this.workspace, 'pkg-'));
    const archive = path.join(directory, 'package.tgz');
    const target = path.join(directory, 'out');

    mkdirSync(target);
    writeFileSync(archive, tarball);
    execFileSync('tar', ['-xzf', archive, '-C', target]);

    const root = path.join(target, 'package');
    const manifest = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8')) as {
      name: string;
      version: string;
      license: string;
    };

    if (manifest.name !== spec.name || manifest.version !== spec.version) {
      throw new Error(
        `Tarball for ${spec.name}@${spec.version} contains ${manifest.name}@${manifest.version}`,
      );
    }

    const licenseName = ['LICENSE', 'LICENSE.md'].find((name) =>
      this.exists(path.join(root, name)),
    );

    this.directories.set(spec.name, root);
    this.records.push({
      ...spec,
      integrity,
      registryUrl,
      license: manifest.license,
      licenseFile: licenseName
        ? { path: licenseName, sha256: sha256(readFileSync(path.join(root, licenseName))) }
        : null,
    });
  }

  private exists(file: string): boolean {
    try {
      readFileSync(file);

      return true;
    } catch {
      return false;
    }
  }

  private read(packageName: string, file: string): string {
    const root = this.directories.get(packageName);

    if (!root) {
      throw new Error(`Package not downloaded: ${packageName}`);
    }

    const content = readFileSync(path.join(root, file), 'utf8');

    if (!this.usedFiles.some((entry) => entry.packageName === packageName && entry.file === file)) {
      this.usedFiles.push({ packageName, file, sha256: sha256(content) });
    }

    return content;
  }

  private realReact(): Record<string, unknown> {
    this.reactModule ??= { ...React, default: React } as Record<string, unknown>;

    return this.reactModule;
  }

  private commonJsRequire(packageName: string, directory: string): (specifier: string) => unknown {
    return (specifier) => {
      if (specifier === 'react') {
        return this.realReact();
      }

      if (specifier === 'react/jsx-runtime') {
        return jsxRuntime;
      }

      if (specifier.startsWith('./')) {
        const file = path.posix.join(
          directory,
          specifier.endsWith('.js') ? specifier : `${specifier}.js`,
        );

        return evaluateCommonJs(
          this.read(packageName, file),
          this.commonJsRequire(packageName, path.posix.dirname(file)),
        );
      }

      throw new Error(`${packageName}: unexpected require("${specifier}")`);
    };
  }

  /** `@hugeicons/react` exports (the legacy `HugeiconsIcon` implementation). */
  hugeiconsReact(): Record<string, unknown> {
    const name = '@hugeicons/react';

    return evaluateCommonJs(
      this.read(name, 'dist/cjs/index.js'),
      this.commonJsRequire(name, 'dist/cjs'),
    ) as Record<string, unknown>;
  }

  /**
   * `@hugeicons/core-free-icons` exports. The package bundles every definition plus alias
   * exports (`BatteryChargingIcon` = `BatteryCharging01Icon`) in `dist/cjs/index.js`, so the whole
   * bundle is evaluated once and the names the Bar imports are recorded.
   */
  hugeiconsCore(): Record<string, unknown> {
    const name = '@hugeicons/core-free-icons';
    const source = this.read(name, 'dist/cjs/index.js');
    const all = evaluateCommonJs(source, () => {
      throw new Error('unexpected require');
    }) as Record<string, unknown>;

    return new Proxy(
      {},
      {
        get: (_target, property) => {
          if (typeof property !== 'string') {
            return undefined;
          }

          if (!(property in all)) {
            throw new Error(`${name} has no export ${property}`);
          }

          this.usedHugeicons.set(property, aliasTarget(source, property));

          return all[property];
        },
      },
    );
  }

  /** Unpacked root of a downloaded package. */
  root(packageName: string): string {
    const root = this.directories.get(packageName);

    if (!root) {
      throw new Error(`Package not downloaded: ${packageName}`);
    }

    return root;
  }

  /** Every `@hugeicons/core-free-icons` export, without recording usage (comparisons only). */
  peekHugeicons(): Record<string, unknown> {
    return evaluateCommonJs(this.read('@hugeicons/core-free-icons', 'dist/cjs/index.js'), () => {
      throw new Error('unexpected require');
    }) as Record<string, unknown>;
  }

  /** Lazily loads one `@icons-pack/react-simple-icons` component. */
  simpleIcons(): Record<string, unknown> {
    const name = '@icons-pack/react-simple-icons';
    const cache = new Map<string, unknown>();

    return new Proxy(
      {},
      {
        get: (_target, property) => {
          if (typeof property !== 'string') {
            return undefined;
          }

          if (!cache.has(property)) {
            const exports = evaluateCommonJs(
              this.read(name, `icons/${property}.cjs`),
              this.commonJsRequire(name, 'icons'),
            ) as { default: unknown };

            cache.set(property, exports.default);
          }

          return cache.get(property);
        },
      },
    );
  }

  /** Reads an icon source file from another installed tree (used for the version comparison). */
  static readFileFrom(root: string, file: string): string | null {
    try {
      return readFileSync(path.join(root, file), 'utf8');
    } catch {
      return null;
    }
  }

  dispose(): void {
    rmSync(this.workspace, { recursive: true, force: true });
  }
}
