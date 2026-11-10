/**
 * Exhaustive, DOM-derived capture of a rendered Bar plus an exact comparison.
 *
 * The same function captures the independent legacy reference and the new Bar, so every
 * difference is reported as data (category, element path, property, expected vs actual). Nothing
 * here rounds, thresholds or masks: values are compared as the browser reports them.
 */

export type DiffCategory = 'structure' | 'attributes' | 'text' | 'geometry' | 'style' | 'svg';

export interface Diff {
  category: DiffCategory;
  path: string;
  property: string;
  expected: string;
  actual: string;
}

type Rect = [number, number, number, number];

export interface ElementCapture {
  path: string;
  tag: string;
  inSvg: boolean;
  /** Attributes except `class`, `id` (container only) and `style`. */
  attributes: Record<string, string>;
  /** Custom properties written inline (`--scroll-*`), which computed style does not enumerate. */
  customProperties: Record<string, string>;
  texts: { data: string; rects: Rect[] }[];
  rect: Rect;
  layout: Record<string, number>;
  style: Record<string, string>;
  pseudo: Record<string, Record<string, string>>;
}

export interface Capture {
  /** `@keyframes` rules by name (a build-scoped `-hash` suffix removed), as their serialized rules. */
  keyframes: Record<string, string[]>;
  viewport: { width: number; height: number; devicePixelRatio: number };
  elements: ElementCapture[];
}

const SVG_NS = 'http://www.w3.org/2000/svg';
const CUSTOM_PROPERTIES = [
  '--button-background-color',
  '--button-border-color',
  '--scroll-start',
  '--scroll-end',
  '--scroll-duration',
];
const LAYOUT_KEYS = [
  'offsetWidth',
  'offsetHeight',
  'offsetLeft',
  'offsetTop',
  'clientWidth',
  'clientHeight',
  'scrollWidth',
  'scrollHeight',
] as const;
/** The page shell above the Bar differs by construction (`.root` vs `#root`), so only layout is compared. */
const SHELL_DEPTH = 3;

function rectOf(rect: DOMRect | DOMRectReadOnly): Rect {
  return [rect.x, rect.y, rect.width, rect.height];
}

/**
 * Legacy-only computed aliases: the reference's flattener does not generate Stylis' vendor-prefixed
 * duplicates (`-webkit-box-align` for `align-items: center`, `-webkit-box-pack` for
 * `justify-content`), while the new build's Stylis does, as the legacy bundle's did. They only
 * affect `display: -webkit-box`, which no Bar rule uses; the unprefixed properties are compared.
 * Reported to the user as a reference gap; nothing else is ignored.
 */
export const LEGACY_PREFIX_ALIASES: readonly string[] = ['-webkit-box-align', '-webkit-box-pack'];

/**
 * Linaria scopes `@keyframes` names with a build hash (`scroll-text-sbwbext`); the reference keeps
 * the source name. The name is an identifier, the rules are compared separately.
 */
function normalizeScopedName(property: string, value: string): string {
  return property === 'animation-name' ? value.replace(/^(scroll-text)-[a-z0-9]+$/u, '$1') : value;
}

function keyframesOf(document: Document): Capture['keyframes'] {
  const result: Capture['keyframes'] = {};

  for (const sheet of document.styleSheets) {
    for (const rule of sheet.cssRules) {
      if (rule instanceof (document.defaultView as Window & typeof globalThis).CSSKeyframesRule) {
        const name = rule.name.replace(/^(scroll-text)-[a-z0-9]+$/u, '$1');

        result[name] = [...rule.cssRules].map((frame) => frame.cssText);
      }
    }
  }

  return result;
}

function computed(window: Window, element: Element, pseudo?: string): Record<string, string> {
  const style = window.getComputedStyle(element, pseudo);
  const result: Record<string, string> = {};

  for (let index = 0; index < style.length; index++) {
    const name = style.item(index);

    if (!LEGACY_PREFIX_ALIASES.includes(name)) {
      result[name] = normalizeScopedName(name, style.getPropertyValue(name));
    }
  }

  for (const name of CUSTOM_PROPERTIES) {
    const value = style.getPropertyValue(name);

    if (value !== '') {
      result[name] = value.trim();
    }
  }

  return result;
}

export function captureDocument(document: Document): Capture {
  const window = document.defaultView;

  if (!window) {
    throw new Error('Document has no window');
  }

  const elements: ElementCapture[] = [];

  // WebKit updates the scrollable overflow of ancestors lazily when a paused transform animation is
  // moved to another time; a throwaway relayout makes `scrollHeight` reflect the current frame.
  // The style is restored immediately and only layout inputs of the shell are touched.
  const root = document.documentElement;

  root.style.paddingBottom = '1px';
  void root.offsetHeight;
  root.style.paddingBottom = '';
  void root.offsetHeight;

  const visit = (element: Element, path: string, depth: number) => {
    const inSvg = element.namespaceURI === SVG_NS;
    const isShell = depth < SHELL_DEPTH;
    const attributes: Record<string, string> = {};

    if (!isShell) {
      for (const attribute of element.attributes) {
        if (attribute.name === 'class' || attribute.name === 'style') {
          continue;
        }

        attributes[attribute.name] = attribute.value;
      }
    }

    const customProperties: Record<string, string> = {};

    for (const name of CUSTOM_PROPERTIES) {
      const value = (element as HTMLElement).style?.getPropertyValue(name);

      if (value) {
        customProperties[name] = value;
      }
    }

    // Text nodes are compared one by one: the client DOM of a JSX `{usage}%` has two nodes and so
    // must the reference's (see `splitLegacyInterpolatedText` in harness.tsx).
    const texts: ElementCapture['texts'] = [];

    for (const node of element.childNodes) {
      if (node.nodeType !== Node.TEXT_NODE || !node.textContent?.trim()) {
        continue;
      }

      const range = document.createRange();

      range.selectNodeContents(node);
      texts.push({
        data: node.textContent,
        rects: [...range.getClientRects()].map((rect) => rectOf(rect)),
      });
    }

    const layout: Record<string, number> = {};

    if (element instanceof window.HTMLElement) {
      for (const key of LAYOUT_KEYS) {
        layout[key] = element[key];
      }
    }

    const pseudo: ElementCapture['pseudo'] = {};

    for (const name of ['::before', '::after']) {
      const style = computed(window, element, name);

      if (style.content !== 'none' && style.content !== 'normal') {
        pseudo[name] = style;
      }
    }

    elements.push({
      path,
      tag: element.localName,
      inSvg,
      attributes,
      customProperties,
      texts,
      rect: rectOf(element.getBoundingClientRect()),
      layout,
      style: computed(window, element),
      pseudo,
    });

    let index = 0;

    for (const child of element.children) {
      if (child.localName === 'head') {
        continue;
      }

      visit(child, `${path}>${child.localName}[${index++}]`, depth + 1);
    }
  };

  const html = document.documentElement;

  visit(html, 'html', 0);

  return {
    keyframes: keyframesOf(document),
    viewport: {
      width: window.innerWidth,
      height: window.innerHeight,
      devicePixelRatio: window.devicePixelRatio,
    },
    elements,
  };
}

function diffRecords(
  category: DiffCategory,
  path: string,
  expected: Record<string, string | number>,
  actual: Record<string, string | number>,
  prefix = '',
): Diff[] {
  const diffs: Diff[] = [];

  for (const key of new Set([...Object.keys(expected), ...Object.keys(actual)])) {
    const left = expected[key];
    const right = actual[key];

    if (left !== right) {
      diffs.push({
        category,
        path,
        property: prefix + key,
        expected: String(left),
        actual: String(right),
      });
    }
  }

  return diffs;
}

function sameRects(left: Rect[], right: Rect[]): boolean {
  return (
    left.length === right.length &&
    left.every((rect, i) => rect.every((v, j) => v === right[i]?.[j]))
  );
}

/** Compares two captures exactly; the first capture is the reference (expected). */
export function compareCaptures(expected: Capture, actual: Capture): Diff[] {
  const diffs: Diff[] = [];

  if (
    expected.viewport.width !== actual.viewport.width ||
    expected.viewport.height !== actual.viewport.height ||
    expected.viewport.devicePixelRatio !== actual.viewport.devicePixelRatio
  ) {
    diffs.push({
      category: 'geometry',
      path: 'viewport',
      property: 'size/scale',
      expected: JSON.stringify(expected.viewport),
      actual: JSON.stringify(actual.viewport),
    });
  }

  for (const name of new Set([
    ...Object.keys(expected.keyframes),
    ...Object.keys(actual.keyframes),
  ])) {
    const left = JSON.stringify(expected.keyframes[name] ?? null);
    const right = JSON.stringify(actual.keyframes[name] ?? null);

    if (left !== right) {
      diffs.push({
        category: 'style',
        path: 'document',
        property: `@keyframes ${name}`,
        expected: left,
        actual: right,
      });
    }
  }

  const actualByPath = new Map(actual.elements.map((element) => [element.path, element]));
  const expectedPaths = new Set(expected.elements.map((element) => element.path));

  for (const element of expected.elements) {
    const other = actualByPath.get(element.path);

    if (!other) {
      diffs.push({
        category: 'structure',
        path: element.path,
        property: 'element',
        expected: element.tag,
        actual: 'missing',
      });
      continue;
    }

    const svg = element.inSvg ? 'svg' : 'attributes';

    diffs.push(
      ...diffRecords(svg, element.path, element.attributes, other.attributes, 'attr:'),
      ...diffRecords(svg, element.path, element.customProperties, other.customProperties, 'var:'),
      ...diffRecords(
        'geometry',
        element.path,
        { rect: element.rect.join(',') },
        { rect: other.rect.join(',') },
      ),
      ...diffRecords('geometry', element.path, element.layout, other.layout),
      ...diffRecords('style', element.path, element.style, other.style),
    );

    if (element.texts.length !== other.texts.length) {
      diffs.push({
        category: 'text',
        path: element.path,
        property: 'text-nodes',
        expected: String(element.texts.length),
        actual: String(other.texts.length),
      });
    } else {
      element.texts.forEach((text, index) => {
        const peer = other.texts[index];

        if (text.data !== peer?.data) {
          diffs.push({
            category: 'text',
            path: element.path,
            property: `text[${index}]`,
            expected: text.data,
            actual: String(peer?.data),
          });
        } else if (!sameRects(text.rects, peer.rects)) {
          diffs.push({
            category: 'geometry',
            path: element.path,
            property: `text[${index}].rects`,
            expected: JSON.stringify(text.rects),
            actual: JSON.stringify(peer.rects),
          });
        }
      });
    }

    for (const name of new Set([...Object.keys(element.pseudo), ...Object.keys(other.pseudo)])) {
      const left = element.pseudo[name];
      const right = other.pseudo[name];

      if (!left || !right) {
        diffs.push({
          category: 'style',
          path: element.path,
          property: name,
          expected: left ? 'generated' : 'none',
          actual: right ? 'generated' : 'none',
        });
      } else {
        diffs.push(...diffRecords('style', element.path, left, right, `${name}:`));
      }
    }
  }

  for (const element of actual.elements) {
    if (!expectedPaths.has(element.path)) {
      diffs.push({
        category: 'structure',
        path: element.path,
        property: 'element',
        expected: 'missing',
        actual: element.tag,
      });
    }
  }

  return diffs;
}

export function describeDiffs(diffs: readonly Diff[], limit = 12): string[] {
  return diffs
    .slice(0, limit)
    .map(
      (diff) =>
        `[${diff.category}] ${diff.path} ${diff.property}: expected ${diff.expected} actual ${diff.actual}`,
    )
    .concat(diffs.length > limit ? [`... and ${diffs.length - limit} more`] : []);
}
