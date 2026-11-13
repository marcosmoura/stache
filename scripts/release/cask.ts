/**
 * Homebrew cask rendering (`RELEASE-05-01`, `RELEASE-05-02`).
 *
 * `renderCask` is the production rendering `runRelease` calls: it takes only the authoritative
 * release config, version and DMG checksum, so no override can reach it. `renderTestCask` is the
 * separate, test-only rendering for private candidate rehearsals: an overridden test-cask
 * version and a loopback URL, never written into `release.json`, the app or the manifests.
 *
 * Test-cask CLI: `bun scripts/release/cask.ts --test-version 0.26.0-test.1 --url <loopback URL>
 * --dmg <path to the candidate DMG> [--out <file>]`.
 */
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { artifactNames, readReleaseConfig, ReleaseError } from './pipeline';

export const caskToken = 'stache';
export const caskRepository = 'marcosmoura/stache';

/**
 * The macOS 26 requirement. Homebrew names macOS 26 `:tahoe`; `brew audit --cask --online`
 * confirms it against `LSMinimumSystemVersion` after promotion (Part 06).
 */
export const caskMacosRequirement = '">= :tahoe"';

export interface CaskInputs {
  productName: string;
  identifier: string;
  version: string;
  sha256: string;
}

function stanzas(
  { productName, identifier, version, sha256 }: CaskInputs,
  url: string,
  extraComment = '',
) {
  if (!/^[0-9a-f]{64}$/.test(sha256)) {
    throw new ReleaseError(`not a SHA-256 checksum: ${sha256}`);
  }

  return `${extraComment}cask "${caskToken}" do
  version "${version}"
  sha256 "${sha256}"

  url "${url}"
  name "${productName}"
  desc "Tray-only macOS enhancement suite"
  homepage "https://github.com/${caskRepository}"

  depends_on arch: :arm64
  depends_on macos: ${caskMacosRequirement}

  app "${productName}.app"
  binary "#{appdir}/${productName}.app/Contents/MacOS/stache", target: "stache"

  uninstall signal:     [["TERM", "${identifier}"]],
            on_upgrade: :signal

  zap trash: "~/Library/Caches/${identifier}"
end
`;
}

/** The production cask: the authoritative version and the DMG's real checksum. */
export function renderCask(inputs: CaskInputs): string {
  const dmg = artifactNames(inputs.productName, '#{version}').dmg;

  return stanzas(
    inputs,
    `https://github.com/${caskRepository}/releases/download/v#{version}/${dmg}`,
  );
}

function isLoopback(url: string): boolean {
  let parsed: URL;

  try {
    parsed = new URL(url);
  } catch {
    return false;
  }

  return (
    ['http:', 'https:'].includes(parsed.protocol) &&
    ['127.0.0.1', '[::1]', 'localhost'].includes(parsed.hostname)
  );
}

/** A local test cask: overridden test version and loopback URL, the candidate's real checksum. */
export function renderTestCask(
  inputs: Omit<CaskInputs, 'version'>,
  testVersion: string,
  url: string,
): string {
  if (!/^\d+\.\d+\.\d+-test\.\d+$/.test(testVersion)) {
    throw new ReleaseError(`test-cask version must look like 0.26.0-test.1, got: ${testVersion}`);
  }

  if (!isLoopback(url)) {
    throw new ReleaseError(`test-cask URL must be loopback, got: ${url}`);
  }

  return stanzas(
    { ...inputs, version: testVersion },
    url,
    '# LOCAL TEST CASK for a private candidate rehearsal. Never publish.\n',
  );
}

export function sha256File(path: string): string {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

function main(args: string[]) {
  const options = new Map<string, string>();

  for (let index = 0; index < args.length; index += 2) {
    const [name, value] = [args[index], args[index + 1]];

    if (!['--test-version', '--url', '--dmg', '--out'].includes(name) || value === undefined) {
      throw new ReleaseError(
        'usage: bun scripts/release/cask.ts --test-version <x.y.z-test.n> --url <loopback URL> --dmg <path> [--out <file>]',
      );
    }

    options.set(name, value);
  }

  const [testVersion, url, dmg] = ['--test-version', '--url', '--dmg'].map((name) =>
    options.get(name),
  );

  if (!testVersion || !url || !dmg) {
    throw new ReleaseError('--test-version, --url and --dmg are all required');
  }

  const conf = readReleaseConfig(
    readFileSync(resolve(import.meta.dirname, '../../src/crates/app/tauri.conf.json'), 'utf8'),
  );
  const text = renderTestCask(
    { productName: conf.productName, identifier: conf.identifier, sha256: sha256File(dmg) },
    testVersion,
    url,
  );
  const out = options.get('--out');

  if (out) {
    writeFileSync(out, text);
  } else {
    process.stdout.write(text);
  }
}

if (import.meta.main) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(
      `test cask failed: ${error instanceof ReleaseError ? error.message : String(error)}`,
    );
    process.exit(1);
  }
}
