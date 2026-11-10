/**
 * Isolated module loader for the pinned legacy sources.
 *
 * Legacy TypeScript is read through `git cat-file`, transpiled with Bun's transpiler (types
 * stripped, classic JSX) and evaluated by a tiny CommonJS shim. Every import is resolved either to
 * another pinned legacy file or to an explicit stub declared in `stubs.ts`. Nothing is read from the
 * `_legacy/` working copy and nothing is resolved through the legacy dependency graph.
 */
import { createHash } from 'node:crypto';
import path from 'node:path';

interface BunTranspiler {
  transformSync(code: string): string;
}

declare const Bun: {
  Transpiler: new (options: {
    loader: 'tsx' | 'ts';
    tsconfig: { compilerOptions: Record<string, unknown> };
  }) => BunTranspiler;
};

const LEGACY_UI_ROOT = '_legacy/app/ui';

export interface LoadedSource {
  path: string;
  blob: string;
  sha256: string;
}

export interface LoaderOptions {
  /** Legacy path -> git blob id, for every file under `_legacy/app/ui` at the pinned commit. */
  blobs: ReadonlyMap<string, string>;
  /** Reads a blob through `git cat-file blob`. */
  readBlob: (blob: string) => string;
  /** Specifier (bare or `@/` alias) -> exports, optionally depending on the importing module. */
  stubs: Readonly<Record<string, (importer: string) => unknown>>;
  /** Resolved legacy path -> exports, replacing a pinned file with a declared stub. */
  pathStubs: Readonly<Record<string, unknown>>;
  /** External (non-legacy) modules that are loaded from real packages. */
  externals: Readonly<Record<string, () => unknown>>;
}

type Req = (specifier: string) => unknown;

export class LegacyLoader {
  readonly loaded = new Map<string, LoadedSource>();
  readonly usedStubs = new Set<string>();
  readonly sources = new Map<string, string>();
  private readonly cache = new Map<string, unknown>();
  private readonly transpiler = new Bun.Transpiler({
    loader: 'tsx',
    tsconfig: {
      compilerOptions: {
        jsx: 'react',
        jsxFactory: 'React.createElement',
        jsxFragmentFactory: 'React.Fragment',
      },
    },
  });

  private readonly options: LoaderOptions;

  constructor(options: LoaderOptions) {
    this.options = options;
  }

  /** Loads a legacy module by its repository path and returns its exports. */
  load(legacyPath: string): Record<string, unknown> {
    const stub = this.options.pathStubs[legacyPath];

    if (stub !== undefined) {
      this.usedStubs.add(legacyPath);

      return stub as Record<string, unknown>;
    }

    const cached = this.cache.get(legacyPath);

    if (cached) {
      return cached as Record<string, unknown>;
    }

    const blob = this.options.blobs.get(legacyPath);

    if (!blob) {
      throw new Error(`Legacy file is not in the pinned commit: ${legacyPath}`);
    }

    const source = this.options.readBlob(blob);

    this.loaded.set(legacyPath, {
      path: legacyPath,
      blob,
      sha256: createHash('sha256').update(source).digest('hex'),
    });
    this.sources.set(legacyPath, source);

    const moduleExports: Record<string, unknown> = {};

    this.cache.set(legacyPath, moduleExports);

    const code = esmToFunctionBody(this.transpiler.transformSync(source), legacyPath);
    const req: Req = (specifier) => this.resolve(specifier, legacyPath);

    new Function('React', '__req', '__exp', code)(
      this.resolve('react', legacyPath),
      req,
      moduleExports,
    );

    return moduleExports;
  }

  private resolve(specifier: string, importer: string): unknown {
    const stub = this.options.stubs[specifier];

    if (stub) {
      this.usedStubs.add(specifier);

      return stub(importer);
    }

    const external = this.options.externals[specifier];

    if (external) {
      return external();
    }

    let target: string;

    if (specifier.startsWith('@/')) {
      target = `${LEGACY_UI_ROOT}/${specifier.slice(2)}`;
    } else if (specifier.startsWith('.')) {
      target = path.posix.join(path.posix.dirname(importer), specifier);
    } else {
      throw new Error(`Unresolved bare import "${specifier}" from ${importer}; declare a stub`);
    }

    const candidates = [
      target,
      `${target}.ts`,
      `${target}.tsx`,
      `${target}/index.ts`,
      `${target}/index.tsx`,
    ];
    const found = candidates.find(
      (candidate) => this.options.blobs.has(candidate) || candidate in this.options.pathStubs,
    );

    if (!found) {
      throw new Error(`Cannot resolve "${specifier}" from ${importer}`);
    }

    return this.load(found);
  }
}

function parseNamed(list: string): [string, string][] {
  return list
    .split(',')
    .map((part) => part.trim())
    .filter((part) => part !== '')
    .map((part) => {
      const [imported, local] = part.split(/\s+as\s+/);

      return [imported, local ?? imported];
    });
}

/**
 * Rewrites the regular output of Bun's transpiler (ESM) into a function body using `__req` and
 * `__exp`. Only the statement shapes the pinned sources use are supported; anything else throws.
 */
export function esmToFunctionBody(js: string, origin: string): string {
  const tail: string[] = [];

  let code = js.replace(
    /^import\s+"([^"]+)";?$/gm,
    (_m, from: string) => `__req(${JSON.stringify(from)});`,
  );

  code = code.replace(
    /^import\s+([\s\S]+?)\s+from\s+"([^"]+)";?$/gm,
    (_m, clause: string, from: string) => {
      const req = `__req(${JSON.stringify(from)})`;
      const statements: string[] = [];
      let rest = clause.trim();

      const namespace = /^\*\s+as\s+(\w+)$/.exec(rest);

      if (namespace) {
        return `const ${namespace[1]} = ${req};`;
      }

      const defaultMatch = /^(\w+)\s*(?:,\s*)?/.exec(rest);

      if (defaultMatch && !rest.startsWith('{')) {
        statements.push(`const ${defaultMatch[1]} = ${req}.default;`);
        rest = rest.slice(defaultMatch[0].length).trim();
      }

      if (rest.startsWith('{')) {
        const named = parseNamed(rest.slice(1, rest.lastIndexOf('}')))
          .map(([imported, local]) => (imported === local ? imported : `${imported}: ${local}`))
          .join(', ');

        statements.push(`const { ${named} } = ${req};`);
      } else if (rest !== '') {
        throw new Error(`Unsupported import clause "${clause}" in ${origin}`);
      }

      return statements.join('\n');
    },
  );

  code = code.replace(/^export\s+\*\s+from\s+"([^"]+)";?$/gm, (_m, from: string) => {
    return `{ const __m = __req(${JSON.stringify(from)}); for (const __k of Object.keys(__m)) { if (__k !== 'default') Object.defineProperty(__exp, __k, { enumerable: true, get: () => __m[__k] }); } }`;
  });

  code = code.replace(
    /^export\s+\{([\s\S]*?)\}\s+from\s+"([^"]+)";?$/gm,
    (_m, list: string, from: string) =>
      `__req(${JSON.stringify(from)});\n${parseNamed(list)
        .map(
          ([imported, local]) =>
            `Object.defineProperty(__exp, ${JSON.stringify(local)}, { enumerable: true, get: () => __req(${JSON.stringify(from)})[${JSON.stringify(imported)}] });`,
        )
        .join('\n')}`,
  );

  code = code.replace(/^export\s+\{([\s\S]*?)\};?$/gm, (_m, list: string) => {
    for (const [local, exported] of parseNamed(list)) {
      tail.push(`__exp.${exported} = ${local};`);
    }

    return '';
  });

  code = code.replace(/^export\s+default\s+/gm, '__exp.default = ');

  code = code.replace(
    /^export\s+((?:async\s+)?function\*?|class|const|let|var)\s+(\w+)/gm,
    (_m, kind: string, name: string) => {
      tail.push(`__exp.${name} = ${name};`);

      return `${kind} ${name}`;
    },
  );

  if (/^\s*export\s/m.test(code) || /^\s*import\s/m.test(code)) {
    throw new Error(`Unsupported module syntax left in ${origin}`);
  }

  return `${code}\n${tail.join('\n')}`;
}

/** Evaluates a CommonJS file from a downloaded npm package with an explicit `require`. */
export function evaluateCommonJs(code: string, req: Req): unknown {
  const module: { exports: unknown } = { exports: {} };

  new Function('require', 'module', 'exports', code)(req, module, module.exports);

  return module.exports;
}
