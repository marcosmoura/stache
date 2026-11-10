import { execFileSync } from 'node:child_process';
/**
 * Bar legacy visual reference extraction (manual, deterministic, idempotent).
 *
 *   bun scripts/bar-reference/extract.ts
 *
 * Reads the legacy Bar sources only through `git cat-file` from the pinned commit, resolves the
 * Linaria styles to static CSS, renders the legacy components to static HTML with the exact legacy
 * icon packages and writes `src/ui/tests/bar-reference/`. It never installs, builds or imports the
 * legacy dependency graph, and never reads the `_legacy/` working copy. See
 * `src/ui/tests/bar-reference/README.md`.
 */
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { createElement, type ComponentType, type ElementType } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

import { flattenCss, serializeRules } from './css.ts';
import { LegacyLoader, evaluateCommonJs } from './loader.ts';
import {
  IconPackages,
  aliasTarget,
  LEGACY_ICON_PACKAGES,
  lockIntegrity,
  type PackageSpec,
} from './npm.ts';
import { FROZEN_CLOCK_LABEL, buildScenarios, contextFor, baseContext } from './scenarios.ts';
import { createStubs, type CssRule, type RenderContext } from './stubs.ts';

process.env.TZ = 'UTC';

const REPOSITORY_ROOT = fileURLToPath(new URL('../..', import.meta.url)).replace(/\/+$/, '');
const OUTPUT_DIRECTORY = path.join(REPOSITORY_ROOT, 'src/ui/tests/bar-reference');
const TOOL_DIRECTORY = path.join(REPOSITORY_ROOT, 'scripts/bar-reference');

const PINNED_COMMIT = 'e0b4648ed22891d3ba29abf3d8fa8f56f949c657';
const LEGACY_UI = '_legacy/app/ui';
const LOCKFILE = '_legacy/pnpm-lock.yaml';

/** Object table from `docs/specs/2026-11-03-bar/04-ui-shell-parity.md`. */
const PINNED_OBJECTS = [
  { path: 'ui/renderer/bar', object: '57d4212a9f591d3230076918a2f6527b7baf5a67' },
  { path: 'ui/stores', object: '2dc990c01a99673cdce2260f577896283f1d909a' },
  { path: 'ui/design-system', object: 'b8af6c5b4cdd411d5a4c752b89c18dcff772d050' },
  { path: 'ui/components', object: 'f935047c48d6cb595f12f0ea24a98327bb41bc55' },
  { path: 'ui/main.css', object: 'd9080d2880fc419ada4133530a2eec31ec4d18b8' },
  { path: 'native/src/modules/bar', object: '093dc10ac555e036f70eb71c520653bcea6f9539' },
] as const;

function git(args: string[]): Buffer {
  return execFileSync('git', ['-c', 'core.fsmonitor=false', ...args], {
    cwd: REPOSITORY_ROOT,
    maxBuffer: 1 << 28,
  });
}

function sha256(content: Buffer | string): string {
  return createHash('sha256').update(content).digest('hex');
}

function verifyPins(): { path: string; expected: string; actual: string }[] {
  const type = git(['cat-file', '-t', PINNED_COMMIT]).toString().trim();
  const commit = git(['rev-parse', `${PINNED_COMMIT}^{commit}`])
    .toString()
    .trim();

  if (type !== 'commit' || commit !== PINNED_COMMIT) {
    throw new Error(`Pinned commit ${PINNED_COMMIT} is missing or not a commit`);
  }

  return PINNED_OBJECTS.map(({ path: objectPath, object }) => {
    const actual = git(['rev-parse', `${PINNED_COMMIT}:_legacy/app/${objectPath}`])
      .toString()
      .trim();

    if (actual !== object) {
      throw new Error(
        `Pinned object mismatch for ${objectPath}: expected ${object}, found ${actual}`,
      );
    }

    return { path: `_legacy/app/${objectPath}`, expected: object, actual };
  });
}

function listBlobs(): Map<string, string> {
  const blobs = new Map<string, string>();
  const output = git(['ls-tree', '-r', '-z', PINNED_COMMIT, LEGACY_UI]).toString();

  for (const entry of output.split('\0').filter(Boolean)) {
    const [meta, file] = entry.split('\t');
    const [, type, blob] = meta.split(' ');

    if (type === 'blob') {
      blobs.set(file, blob);
    }
  }

  return blobs;
}

function readBlob(blob: string): string {
  return git(['cat-file', 'blob', blob]).toString('utf8');
}

function pinnedBy(file: string): string {
  const prefix = '_legacy/app/';
  const relative = file.startsWith(prefix) ? file.slice(prefix.length) : file;
  const entry = PINNED_OBJECTS.find(
    (object) => relative === object.path || relative.startsWith(`${object.path}/`),
  );

  return entry ? `table:${entry.path}` : 'commit-only';
}

// -- Font record (measurement host, informational only) -----------------------------------------

function readFontRecord(): Record<string, unknown> | null {
  const file = path.join(os.homedir(), 'Library/Fonts/MapleMono-NF-Medium.ttf');
  let buffer: Buffer;

  try {
    buffer = readFileSync(file);
  } catch {
    return null;
  }

  const tableCount = buffer.readUInt16BE(4);
  const names: Record<number, string> = {};

  for (let index = 0; index < tableCount; index++) {
    const record = 12 + index * 16;

    if (buffer.toString('ascii', record, record + 4) !== 'name') {
      continue;
    }

    const offset = buffer.readUInt32BE(record + 8);
    const count = buffer.readUInt16BE(offset + 2);
    const storage = offset + buffer.readUInt16BE(offset + 4);

    for (let n = 0; n < count; n++) {
      const entry = offset + 6 + n * 12;
      const platform = buffer.readUInt16BE(entry);
      const nameId = buffer.readUInt16BE(entry + 6);

      if (platform !== 3 || ![1, 4, 5].includes(nameId) || nameId in names) {
        continue;
      }

      const length = buffer.readUInt16BE(entry + 8);
      const start = storage + buffer.readUInt16BE(entry + 10);

      names[nameId] = Buffer.from(buffer.subarray(start, start + length))
        .swap16()
        .toString('utf16le');
    }
  }

  return {
    family: names[1] ?? null,
    fullName: names[4] ?? null,
    version: names[5] ?? null,
    fileName: path.basename(file),
    sha256: sha256(buffer),
  };
}

// -- Rendering ----------------------------------------------------------------------------------

function render(element: ReturnType<typeof createElement>): string {
  return renderToStaticMarkup(element);
}

function hashFile(file: string): string {
  return sha256(readFileSync(file));
}

async function main(): Promise<void> {
  const pins = verifyPins();
  const blobs = listBlobs();

  const lockBlob = git(['rev-parse', `${PINNED_COMMIT}:${LOCKFILE}`])
    .toString()
    .trim();
  const lockfile = readBlob(lockBlob);

  const packages = new IconPackages();

  try {
    const specs: (PackageSpec & { integrity: string })[] = LEGACY_ICON_PACKAGES.map((spec) => ({
      ...spec,
      integrity: lockIntegrity(lockfile, spec),
    }));

    for (const spec of specs) {
      await packages.download(spec, spec.integrity);
    }

    const cssRules: CssRule[] = [];
    const context = baseContext();
    const holder: { loader: LegacyLoader | null } = { loader: null };

    const { stubs, pathStubs } = createStubs(context, {
      cssRules,
      getSource: (modulePath) => {
        const source = holder.loader?.sources.get(modulePath);

        if (!source) {
          throw new Error(`Source not loaded: ${modulePath}`);
        }

        return source;
      },
      weatherIcons: () => {
        const icons = holder.loader?.load(`${LEGACY_UI}/stores/WeatherStore/WeatherStore.icons.ts`);

        return { getWeatherIcon: icons?.getWeatherIcon };
      },
    });

    const loader = new LegacyLoader({
      blobs,
      readBlob,
      stubs,
      pathStubs,
      externals: {
        '@hugeicons/react': () => packages.hugeiconsReact(),
        '@hugeicons/core-free-icons': () => packages.hugeiconsCore(),
        '@icons-pack/react-simple-icons': () => packages.simpleIcons(),
      },
    });

    holder.loader = loader;

    const barModule = loader.load(`${LEGACY_UI}/renderer/bar/index.ts`) as { Bar: ComponentType };
    const fallbackModule = loader.load(`${LEGACY_UI}/components/QueryErrorFallback/index.ts`) as {
      QueryErrorFallback: ComponentType;
    };
    const iconModule = loader.load(`${LEGACY_UI}/components/Icon/index.ts`) as {
      Icon: ElementType;
    };
    const colors = (
      loader.load(`${LEGACY_UI}/design-system/colors.ts`) as {
        colors: Record<string, string>;
      }
    ).colors;

    // Every other legacy file must sit inside a pinned tree, except the one declared utility.
    const outsideTable = [...loader.loaded.keys()].filter(
      (file) => pinnedBy(file) === 'commit-only',
    );

    if (outsideTable.join() !== `${LEGACY_UI}/utils/media-query.ts`) {
      throw new Error(
        `Unexpected legacy files outside the pinned trees: ${outsideTable.join(', ')}`,
      );
    }

    // -- Fixtures ---------------------------------------------------------------------------
    const assign = (target: RenderContext, source: RenderContext) => Object.assign(target, source);
    const scenarios = buildScenarios();
    const fixtures: Record<string, { description: string; wide: string; compact: string }> = {};

    for (const scenario of scenarios) {
      const html: Record<'wide' | 'compact', string> = { wide: '', compact: '' };

      for (const compact of [false, true]) {
        assign(context, contextFor(scenario.patch, compact));
        const element = scenario.patch.errorFallback
          ? createElement(fallbackModule.QueryErrorFallback)
          : createElement(barModule.Bar);

        html[compact ? 'compact' : 'wide'] = render(element);
      }

      fixtures[scenario.name] = { description: scenario.description, ...html };
    }

    // Sanity: the legacy clock formatter, run against the frozen instant, must match.
    if (!fixtures['bar-default'].wide.includes(`<span>${FROZEN_CLOCK_LABEL}</span>`)) {
      throw new Error(`Clock label ${FROZEN_CLOCK_LABEL} not rendered; check timezone handling`);
    }

    for (const [name, fixture] of Object.entries(fixtures)) {
      if (/<!--|\$!/.test(fixture.wide + fixture.compact)) {
        throw new Error(`Fixture ${name} contains React stream markers`);
      }
    }

    // -- CSS --------------------------------------------------------------------------------
    const mainCss = readBlob(
      git(['rev-parse', `${PINNED_COMMIT}:${LEGACY_UI}/main.css`])
        .toString()
        .trim(),
    );
    const componentCss = cssRules
      .map((rule) => {
        const stem = rule.module
          .split('/')
          .pop()
          ?.replace(/\.styles\.tsx?$/, '');

        return `/* ${stem}.${rule.name} <- ${rule.module} */\n${serializeRules(flattenCss(rule.className, rule.text))}`;
      })
      .join('\n\n');

    const referenceCss = [
      `/* Bar legacy visual reference. Generated by scripts/bar-reference/extract.ts. Do not edit. */`,
      `/* Source commit ${PINNED_COMMIT}. Section 1 is _legacy/app/ui/main.css verbatim. */`,
      '',
      mainCss.trimEnd(),
      '',
      `/* Section 2: resolved Linaria templates in legacy module evaluation order. */`,
      '',
      componentCss,
      '',
    ].join('\n');

    // -- Icons ------------------------------------------------------------------------------
    const heroIcons = packages.hugeiconsCore();
    const simpleIcons = packages.simpleIcons();
    const iconSpecs: {
      file: string;
      icon: string;
      library: 'hugeicons' | 'simple';
      props: Record<string, unknown>;
    }[] = [
      {
        file: 'PlayCircle02Icon',
        icon: 'PlayCircle02Icon',
        library: 'hugeicons',
        props: { size: 22, color: colors.text },
      },
      {
        file: 'SpotifyIcon',
        icon: 'SpotifyIcon',
        library: 'hugeicons',
        props: { size: 22, color: colors.green },
      },
      {
        file: 'YoutubeIcon',
        icon: 'YoutubeIcon',
        library: 'hugeicons',
        props: { size: 22, color: colors.red },
      },
      {
        file: 'SiTidal',
        icon: 'SiTidal',
        library: 'simple',
        props: { size: 18, color: colors.text },
      },
      {
        file: 'Vynil02Icon',
        icon: 'Vynil02Icon',
        library: 'hugeicons',
        props: { size: 22, color: colors.sky },
      },
      ...[
        'BatteryChargingIcon',
        'BatteryEmptyIcon',
        'BatteryFullIcon',
        'BatteryLowIcon',
        'BatteryMedium02Icon',
        'BatteryMediumIcon',
        'Time03Icon',
        'CpuIcon',
        'CpuChargeIcon',
        'CloudAngledRainIcon',
        'CloudAngledRainZapIcon',
        'CloudIcon',
        'CloudSlowWindIcon',
        'FastWindIcon',
        'MoonAngledRainZapIcon',
        'MoonCloudAngledRainIcon',
        'MoonCloudIcon',
        'MoonCloudSnowIcon',
        'MoonIcon',
        'SnowIcon',
        'SunCloud02Icon',
        'SunCloudAngledRainIcon',
        'SunCloudSnowIcon',
        'SunIcon',
        'Wifi01Icon',
        'WifiDisconnected04Icon',
        'WifiFullSignalIcon',
        'WifiLowSignalIcon',
        'WifiMediumSignalIcon',
        'WifiOff02Icon',
      ].map((icon) => ({ file: icon, icon, library: 'hugeicons' as const, props: {} })),
      {
        file: 'Coffee02Icon.inactive',
        icon: 'Coffee02Icon',
        library: 'hugeicons',
        props: { fill: 'transparent' },
      },
      {
        file: 'Coffee02Icon.active',
        icon: 'Coffee02Icon',
        library: 'hugeicons',
        props: { fill: colors.green, color: colors.green },
      },
    ];

    const usedIconNames = new Set([
      ...packages.usedHugeicons.keys(),
      ...packages.usedFiles
        .filter((entry) => /^icons\/Si\w+\.cjs$/.test(entry.file))
        .map((entry) => path.posix.basename(entry.file, '.cjs')),
    ]);
    const tableIcons = new Set(iconSpecs.map((spec) => spec.icon));
    const missing = [...usedIconNames].filter((name) => !tableIcons.has(name));
    const unused = [...tableIcons].filter((name) => !usedIconNames.has(name));

    if (missing.length > 0 || unused.length > 0) {
      throw new Error(
        `Icon table out of sync. Used but untabled: ${missing}; tabled but unused: ${unused}`,
      );
    }

    const coreBundle = packages.usedFiles.find((entry) => entry.file === 'dist/cjs/index.js');
    const icons: Record<string, string> = {};
    const iconRecords = iconSpecs.map((spec) => {
      const definition = (spec.library === 'hugeicons' ? heroIcons : simpleIcons)[spec.icon];
      const svg = render(createElement(iconModule.Icon, { icon: definition, ...spec.props }));

      if (!svg.startsWith('<svg')) {
        throw new Error(`Icon ${spec.file} did not render an <svg>`);
      }

      icons[`${spec.file}.svg`] = `${svg}\n`;

      const isHuge = spec.library === 'hugeicons';
      const sourceFile = isHuge ? 'dist/cjs/index.js' : `icons/${spec.icon}.cjs`;
      const packageName = isHuge ? '@hugeicons/core-free-icons' : '@icons-pack/react-simple-icons';
      const source = isHuge
        ? coreBundle
        : packages.usedFiles.find(
            (entry) => entry.packageName === packageName && entry.file === sourceFile,
          );

      return {
        file: `icons/${spec.file}.svg`,
        icon: spec.icon,
        aliasOf: isHuge ? (packages.usedHugeicons.get(spec.icon) ?? null) : null,
        package: packageName,
        sourceFile,
        sourceSha256: source?.sha256 ?? null,
        definitionSha256: sha256(
          JSON.stringify(definition, (_k, v: unknown) => (typeof v === 'function' ? undefined : v)),
        ),
        renderProps: { size: 18, strokeWidth: 1.8, ...spec.props },
        renderedBy: '_legacy/app/ui/components/Icon/Icon.tsx + legacy icon package component',
        sha256: sha256(icons[`${spec.file}.svg`]),
      };
    });

    // -- Informational: 4.2.3 vs the repository's installed hugeicons --------------------------
    const repoIconRoot = path.join(REPOSITORY_ROOT, 'node_modules/@hugeicons/core-free-icons');
    const repoPackage = JSON.parse(
      readFileSync(path.join(repoIconRoot, 'package.json'), 'utf8'),
    ) as { version: string };
    const repoBundle = readFileSync(path.join(repoIconRoot, 'dist/cjs/index.js'), 'utf8');
    const repoIcons = evaluateCommonJs(repoBundle, () => {
      throw new Error('unexpected require');
    }) as Record<string, unknown>;
    const comparison: Record<
      string,
      { identical: boolean; legacyAlias: string | null; repositoryAlias: string | null }
    > = {};

    for (const name of [...tableIcons].filter((icon) => !icon.startsWith('Si')).sort()) {
      const repoDefinition = repoIcons[name];

      comparison[name] = {
        identical: JSON.stringify(repoDefinition) === JSON.stringify(heroIcons[name]),
        legacyAlias: packages.usedHugeicons.get(name) ?? null,
        repositoryAlias: repoDefinition === undefined ? null : aliasTarget(repoBundle, name),
      };
    }

    // -- Outputs ----------------------------------------------------------------------------
    const fixturesJson = `${JSON.stringify(fixtures, null, 2)}\n`;
    const outputs: Record<string, string> = {
      'reference.css': referenceCss,
      'fixtures/fixtures.json': fixturesJson,
      ...Object.fromEntries(Object.entries(icons).map(([name, svg]) => [`icons/${name}`, svg])),
    };

    const toolFiles = readdirSync(TOOL_DIRECTORY)
      .filter((name) => name.endsWith('.ts') && !name.endsWith('.test.ts'))
      .sort()
      .map((name) => ({
        path: `scripts/bar-reference/${name}`,
        sha256: hashFile(path.join(TOOL_DIRECTORY, name)),
      }));

    const manifest = {
      schema: 1,
      pinnedCommit: PINNED_COMMIT,
      pinnedObjects: pins,
      sources: [...loader.loaded.values()]
        .sort((a, b) => a.path.localeCompare(b.path))
        .map((source) => ({ ...source, pinnedBy: pinnedBy(source.path) })),
      lockfile: {
        path: LOCKFILE,
        blob: lockBlob,
        sha256: sha256(lockfile),
      },
      iconPackages: packages.records.map(
        ({ name, version, integrity, registryUrl, license, licenseFile }) => ({
          name,
          version,
          integrity,
          registryUrl,
          license,
          licenseFile,
          filesRead: packages.usedFiles
            .filter((entry) => entry.packageName === name)
            .sort((a, b) => a.file.localeCompare(b.file))
            .map(({ file, sha256: hash }) => ({ file, sha256: hash })),
        }),
      ),
      icons: iconRecords.sort((a, b) => a.file.localeCompare(b.file)),
      hugeiconsRepositoryComparison: {
        legacyVersion: '4.2.3',
        repositoryVersion: repoPackage.version,
        identicalSourceByIcon: comparison,
      },
      outputs: Object.entries(outputs)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([file, content]) => ({ path: file, sha256: sha256(content) })),
      extractionTool: {
        files: toolFiles,
        sha256: sha256(toolFiles.map((file) => `${file.path}:${file.sha256}`).join('\n')),
      },
      scenarios: scenarios.map((scenario) => scenario.name),
      declaredStubs: [...loader.usedStubs].sort(),
      handReconstructed: HAND_RECONSTRUCTED,
      environment: {
        extractionHost: { font: readFontRecord() },
        parityRun: {
          font: { family: null, version: null, fileSha256: null },
          os: null,
          browser: null,
          viewport: null,
          deviceScale: null,
          colorProfile: null,
          recordedBy: null,
        },
      },
    };

    // -- Write ------------------------------------------------------------------------------
    rmSync(path.join(OUTPUT_DIRECTORY, 'fixtures'), { recursive: true, force: true });
    rmSync(path.join(OUTPUT_DIRECTORY, 'icons'), { recursive: true, force: true });
    rmSync(path.join(OUTPUT_DIRECTORY, 'reference.css'), { force: true });

    for (const [file, content] of Object.entries(outputs)) {
      const target = path.join(OUTPUT_DIRECTORY, file);

      mkdirSync(path.dirname(target), { recursive: true });
      writeFileSync(target, content);
    }

    writeFileSync(
      path.join(OUTPUT_DIRECTORY, 'manifest.json'),
      `${JSON.stringify(manifest, null, 2)}\n`,
    );

    console.log(
      `Wrote ${Object.keys(outputs).length} files, ${scenarios.length} scenarios, ${iconRecords.length} icons, ` +
        `${cssRules.length} style templates, ${loader.loaded.size} legacy sources.`,
    );
    const differing = Object.entries(comparison)
      .filter(([, entry]) => !entry.identical)
      .map(([name]) => name);

    console.log(
      `hugeicons 4.2.3 vs ${repoPackage.version}: differing icons: ${differing.join(', ') || 'none'}`,
    );
  } finally {
    packages.dispose();
  }
}

/** Pieces that are not produced by running the pinned sources unchanged. Keep this list short. */
const HAND_RECONSTRUCTED = [
  {
    piece: 'Surface motion (motion/react)',
    detail:
      'motion is not installed. Surface renders the settled inline opacity:1 (opacity:0 for the bar-fade-initial fixture); the 0.275s ease fade itself is JS-driven and must be sampled by the parity runner.',
  },
  {
    piece: 'ErrorBoundary catch (react-error-boundary)',
    detail:
      'Error boundaries do not run in static rendering. The global-error fixture renders the pinned QueryErrorFallback directly; the boundary wrapper has no DOM of its own.',
  },
  {
    piece: 'ScrollingLabel measured state',
    detail:
      'start/end/distance come from layout measurement in a browser. The *-scrolling fixtures inject fixed values into the pinned component through a useState shim; the parity runner measures real values.',
  },
  {
    piece: 'Media artwork preload state',
    detail:
      'loadedArtwork is set by an Image() load effect. Fixtures inject the loaded (data URI) and failed (null) results. Artwork pixels are a synthetic generated PNG, not captured media.',
  },
  {
    piece: 'Data access boundary',
    detail:
      'React Query, Tauri invoke/events, the useTauri/useWidgetToggle/useMediaQuery hooks and the Weather store plumbing are replaced by fixture data (stubs.ts). Labels, colors, icon choice and markup are computed by the pinned sources.',
  },
  {
    piece: 'Spaces data hook and workspace/app lists',
    detail:
      'Spaces.state, AppList and WorkspaceList are stubbed (tiling absent). Spaces.tsx and its SpacesFallback run unchanged; Spaces/components styles are intentionally not extracted (strip is not ported).',
  },
  {
    piece: 'Styles',
    detail:
      'Linaria is replaced by a template-collecting css/cx stub and a small flattener (css.ts). Stylis vendor-prefix duplicates are not generated. Class names are generated (lr-<Module>-<name>). Rule order follows legacy module evaluation order, which is how the legacy bundle ordered its CSS.',
  },
  {
    piece: 'Clock',
    detail: 'The legacy formatter runs unchanged against a frozen Date in UTC.',
  },
  {
    piece: 'React runtime',
    detail:
      'Rendered with the repository react-dom/server (classic JSX transform); the legacy lockfile pins react 19.2.8.',
  },
] as const;

await main();
