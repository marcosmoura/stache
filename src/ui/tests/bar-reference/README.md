# Bar legacy visual reference

Independent reference for the Bar visual-parity gate (spec `docs/specs/2026-11-03-bar/04-ui-shell-parity.md`,
"Independent reference process", option 1, approved 2026-11-10). It is **data extracted from the pinned
legacy sources**, never produced by the new Bar components or shared new styling helpers, and nothing in
this directory imports from `src/ui/src`.

Pinned commit: `e0b4648ed22891d3ba29abf3d8fa8f56f949c657`.

## Contents

| Path                        | What it is                                                                                |
| --------------------------- | ----------------------------------------------------------------------------------------- |
| `reference.css`             | Legacy `main.css` verbatim, then every Linaria template of the Bar flattened to CSS.      |
| `fixtures/fixtures.json`    | Static HTML of the legacy Bar per visual branch, `wide` and `compact` (`width <= 2036px`) |
| `icons/*.svg`               | Each icon the Bar uses, rendered by the legacy `Icon` + legacy icon package component.    |
| `manifest.json`             | Pins, hashes, icon package provenance, hand-reconstructed list, run-environment slots.    |
| `verify.ts`                 | Pure provenance verifier (SHA-256 of every file, pins, integrity, no new-code refs).      |
| `reference.ts`, `render.ts` | Vite loader and the WebKit render harness for parity tests.                               |
| `bar-reference.test.ts`     | Smoke test: provenance + one fixture mounted with the root font stack asserted.           |

`scripts/bar-reference/provenance.test.ts` (`bun run test:scripts`) re-verifies the same files from disk
and additionally checks the extraction tool hashes.

## Re-running the extraction

```sh
bun scripts/bar-reference/extract.ts
```

Manual only; never part of a test or build. It needs `git` and network access to `registry.npmjs.org`
(for the three icon tarballs). It is deterministic: running it twice on unchanged inputs and tool produces
byte-identical output (only `manifest.json` changes when the tool changes, because it records the tool
hash).

What it reads:

- Legacy sources **only** through `git cat-file blob` from the pinned commit. It first verifies the commit
  and that the commit's tree contains exactly the object IDs of the spec table (bar renderer tree, stores,
  design-system, components, `main.css` blob, native bar module) and fails loudly otherwise. The `_legacy/`
  working copy is never read, nothing is installed or built there, and no legacy dependency graph is
  imported.
- `_legacy/pnpm-lock.yaml` (via git) for the icon package versions and integrity hashes. The exact tarballs
  `@hugeicons/core-free-icons@4.2.3`, `@hugeicons/react@1.1.9` and
  `@icons-pack/react-simple-icons@13.15.1` are downloaded from the registry, their sha512 is checked
  against the lockfile, and they are unpacked into a fresh temporary directory that is deleted afterwards.
- The repository's own `react` and `react-dom/server` (current tooling) to render markup.

How it works (`scripts/bar-reference/`):

1. `loader.ts` transpiles the pinned `.ts`/`.tsx` files with Bun's transpiler and evaluates them in a
   CommonJS shim. Imports resolve only to other pinned legacy files, to a declared stub (`stubs.ts`), or to
   the downloaded icon packages. Anything else throws. The code evaluates pinned legacy sources and
   downloaded packages with `new Function`; run it only on the pinned inputs.
2. The `@linaria/core` stub collects each `export const x = css\`...\`` template with its interpolated
   design tokens (`colors`, `motion`, `CSS_LAPTOP_MEDIA_QUERY`are the real legacy modules). An interpolation
   that is not a string or number throws; nothing is guessed.`css.ts`flattens nesting,`@media`and
   hoists`@keyframes`.
3. `scenarios.ts` lists the visual branches; each is rendered with `react-dom/server` from the real legacy
   `Bar`/`QueryErrorFallback`, so labels, colors, icon choice and DOM come from the legacy code.

Scope note: the workspace/app strip (`Spaces` components and `Spaces.constants` icon maps) is not ported, so
its styles and icons are not extracted. `Spaces.tsx` itself (empty first slot and the `SpacesFallback`
"Loading..." surface) is rendered unchanged.

## Provenance and what is hand-reconstructed

The manifest records: the pinned commit and verified object IDs; blob ID and SHA-256 of every legacy source
file that was read (each tagged with the table entry that pins it, plus `ui/utils/media-query.ts`, which is
pinned by the commit only); the lockfile blob and SHA-256; package name, version, sha512 integrity, license
and the SHA-256 of every package file read; per-icon SVG SHA-256 and source hash; SHA-256 of every output;
the extraction tool hashes; and empty slots for the parity run environment.

`handReconstructed` in the manifest is authoritative. In short, these are not the pinned code running
unmodified:

- **Data boundary**: React Query, Tauri `invoke`/events, `useTauri*`/`useWidgetToggle`/`useMediaQuery` and
  the Weather store plumbing are replaced by fixture data. Synthetic values only, never native readings.
- **`motion`**: not installed; `Surface` renders the settled inline `opacity:1` (and `opacity:0` in
  `bar-fade-initial`). The 0.275s fade is JS-driven and must be sampled by the runner.
- **ErrorBoundary**: static rendering cannot catch; `global-error` renders the pinned `QueryErrorFallback`
  directly.
- **ScrollingLabel / Media artwork**: measurement-derived scroll state and the artwork `Image()` load result
  are injected (`*-scrolling`, `media-artwork-*`). The artwork PNG is generated, not captured media.
- **Spaces data hook and lists** are stubs (tiling absent).
- **Styles**: Linaria is replaced by the collector and flattener; Stylis vendor-prefix duplicates are not
  generated; class names are `lr-<Module>-<name>`; rule order is legacy module evaluation order (Button
  before Surface, as in the bundle). If the real legacy bundle ordered CSS differently this must be
  reviewed.
- **Page shell**: the `html > body > .root` wrapper in `render.ts` mirrors the `.root` selector in `main.css`;
  the legacy `index.html` is outside the pinned table.

Informational: for every `@hugeicons/core-free-icons` icon the Bar uses, the manifest compares the 4.2.3
definition and alias target with the repository's installed 4.4.0 (`hugeiconsRepositoryComparison`).

## Using it in parity tests

```ts
const reference = await mountReference({ fixture: 'media-paused', width: 2035, scale: 1 });
// reference.document is the iframe document; reference.root is the legacy `.root`.
```

The harness mounts into an iframe so the legacy `:root` rules and the `(width <= 2036px)` query evaluate
against the iframe viewport (the Bar panel viewport, `display - 2 * padding`), and picks the `wide` or
`compact` markup from that query exactly as the legacy `useMediaQuery` would. Device scale and color profile
cannot be changed from a page: the runner sets them and records them under `environment.parityRun`.

Maple Mono NF is system-installed and never bundled. The manifest records the font read on the extraction
host (`environment.extractionHost`, informational); the parity run must record its own font name, version
and file hash and treat a missing font as an environment failure.

## Corrections need review

Never regenerate silently. A change to the extraction tool, a different icon package version, a changed
scenario or any diff in the outputs is a reference correction: review the diff against the legacy sources,
update the provenance, and get user approval before the new reference is used to accept parity (spec: "an
extraction correction needs review and provenance update, not silent baseline regeneration"). The
verification test fails when an output file or the tool no longer matches `manifest.json`. The generated
files are excluded from `oxfmt` and `stylelint` so formatting cannot alter their hashes.
