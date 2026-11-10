/**
 * Small, auditable CSS flattener for the Bar reference extraction.
 *
 * The legacy Linaria `css` templates use nesting (`&:hover`, `&::after`, nested `@media` and a
 * nested `@keyframes`). This module turns one template (already interpolated with the legacy
 * design tokens) into flat rules, the way Linaria's stylis pass does, minus vendor prefixing.
 * It only understands the constructs the pinned Bar sources use and throws on anything else.
 */

type Item = { kind: 'decl'; text: string } | { kind: 'block'; head: string; body: Item[] };

interface FlatRule {
  at: string[];
  selectors: string[];
  decls: string[];
}

export interface FlattenedCss {
  rules: FlatRule[];
  keyframes: string[];
}

function stripComments(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, '');
}

function parse(text: string): Item[] {
  let index = 0;

  function readBody(topLevel: boolean): Item[] {
    const items: Item[] = [];
    let buffer = '';
    let quote: string | null = null;

    while (index < text.length) {
      const char = text[index++];

      if (quote) {
        buffer += char;

        if (char === quote) {
          quote = null;
        }

        continue;
      }

      if (char === '"' || char === "'") {
        quote = char;
        buffer += char;
      } else if (char === '{') {
        const head = buffer.trim();

        buffer = '';

        if (head === '') {
          throw new Error('CSS block without a selector');
        }

        items.push({ kind: 'block', head, body: readBody(false) });
      } else if (char === '}') {
        if (topLevel) {
          throw new Error('Unbalanced "}" in CSS template');
        }

        if (buffer.trim() !== '') {
          throw new Error(`Declaration without ";": ${buffer.trim()}`);
        }

        return items;
      } else if (char === ';') {
        const decl = buffer.trim();

        buffer = '';

        if (decl !== '') {
          items.push({ kind: 'decl', text: decl });
        }
      } else {
        buffer += char;
      }
    }

    if (!topLevel) {
      throw new Error('Unbalanced "{" in CSS template');
    }

    if (buffer.trim() !== '') {
      throw new Error(`Declaration without ";": ${buffer.trim()}`);
    }

    return items;
  }

  return readBody(true);
}

function splitSelectorList(head: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let buffer = '';

  for (const char of head) {
    if (char === '(' || char === '[') {
      depth++;
    }

    if (char === ')' || char === ']') {
      depth--;
    }

    if (char === ',' && depth === 0) {
      parts.push(buffer.trim());
      buffer = '';
    } else {
      buffer += char;
    }
  }

  parts.push(buffer.trim());

  return parts.filter((part) => part !== '');
}

function normalizeDeclaration(text: string): string {
  const colon = text.indexOf(':');

  if (colon === -1) {
    throw new Error(`Not a declaration: ${text}`);
  }

  const property = text.slice(0, colon).trim();
  // Non-ASCII characters (the NBSP in a `content` string) become 6-digit CSS escapes, which need
  // no terminating space and keep the generated file ASCII-only.
  const value = text
    .slice(colon + 1)
    .replace(/[ \t\r\n]+/g, ' ')
    .trim()
    .replace(
      /[^\x20-\x7e]/gu,
      (char) => `\\${(char.codePointAt(0) ?? 0).toString(16).padStart(6, '0')}`,
    );

  return `${property}: ${value}`;
}

export function flattenCss(className: string, template: string): FlattenedCss {
  const rules: FlatRule[] = [];
  const keyframes: string[] = [];

  function walk(items: Item[], selectors: string[], at: string[]): void {
    let pending: string[] = [];

    const flush = () => {
      if (pending.length > 0) {
        rules.push({ at, selectors, decls: pending });
        pending = [];
      }
    };

    for (const item of items) {
      if (item.kind === 'decl') {
        pending.push(normalizeDeclaration(item.text));
        continue;
      }

      flush();

      if (item.head.startsWith('@keyframes')) {
        keyframes.push(serializeKeyframes(item.head, item.body));
      } else if (item.head.startsWith('@media')) {
        walk(item.body, selectors, [...at, item.head.replace(/\s+/g, ' ')]);
      } else if (item.head.startsWith('@')) {
        throw new Error(`Unsupported at-rule in reference CSS: ${item.head}`);
      } else {
        const next = splitSelectorList(item.head).flatMap((part) =>
          selectors.map((parent) =>
            part.includes('&') ? part.replaceAll('&', parent) : `${parent} ${part}`,
          ),
        );

        walk(item.body, next, at);
      }
    }

    flush();
  }

  walk(parse(stripComments(template)), [`.${className}`], []);

  return { rules, keyframes };
}

function serializeKeyframes(head: string, body: Item[]): string {
  const frames = body.map((frame) => {
    if (frame.kind !== 'block') {
      throw new Error('Declaration directly inside @keyframes');
    }

    const decls = frame.body.map((item) => {
      if (item.kind !== 'decl') {
        throw new Error('Nested block inside a keyframe');
      }

      return `    ${normalizeDeclaration(item.text)};`;
    });

    return `  ${splitSelectorList(frame.head).join(', ')} {\n${decls.join('\n')}\n  }`;
  });

  return `${head.replace(/\s+/g, ' ')} {\n${frames.join('\n')}\n}`;
}

export function serializeRules(flat: FlattenedCss): string {
  const blocks = flat.rules.map((rule) => {
    const indent = '  '.repeat(rule.at.length);
    const lines = [
      `${indent}${rule.selectors.join(`,\n${indent}`)} {`,
      ...rule.decls.map((decl) => `${indent}  ${decl};`),
      `${indent}}`,
    ];

    return rule.at.reduceRight((inner, query, depth) => {
      const outer = '  '.repeat(depth);

      return `${outer}${query} {\n${inner}\n${outer}}`;
    }, lines.join('\n'));
  });

  return [...blocks, ...flat.keyframes].join('\n\n');
}
