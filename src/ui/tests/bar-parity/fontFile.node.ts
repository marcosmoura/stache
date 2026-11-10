/**
 * Node-only reader for the installed Maple Mono NF font file (the parity run's environment record).
 *
 * Runs in the Vitest node process (a browser command), never in the page. It reads the user font
 * directories, hashes the medium (weight 500) file and extracts the metrics the page-side font check
 * compares against. The font is system-installed measurement input; it is never copied or bundled.
 */
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

export interface FontRecord {
  found: boolean;
  /** Where the lookup happened; recorded so a missing font explains itself. */
  searched: string[];
  fileName: string;
  path?: string;
  sha256?: string;
  family?: string;
  fullName?: string;
  version?: string;
  unitsPerEm?: number;
  ascender?: number;
  descender?: number;
  /** Advance width of the glyph for "H", in font units. */
  advanceH?: number;
}

const FONT_FILE = 'MapleMono-NF-Medium.ttf';

function nameRecords(view: DataView, table: number): Map<number, string> {
  const count = view.getUint16(table + 2);
  const stringOffset = view.getUint16(table + 4);
  const names = new Map<number, string>();

  for (let index = 0; index < count; index++) {
    const record = table + 6 + index * 12;
    const platform = view.getUint16(record);
    const nameId = view.getUint16(record + 6);
    const length = view.getUint16(record + 8);
    const offset = view.getUint16(record + 10);

    // Windows (3) UTF-16BE entries; the first one per id wins.
    if (platform !== 3 || names.has(nameId)) {
      continue;
    }

    let text = '';

    for (let at = 0; at < length; at += 2) {
      text += String.fromCharCode(view.getUint16(table + stringOffset + offset + at));
    }

    names.set(nameId, text);
  }

  return names;
}

function glyphIdFor(view: DataView, cmap: number, codePoint: number): number {
  const tables = view.getUint16(cmap + 2);

  for (let index = 0; index < tables; index++) {
    const subtable = cmap + view.getUint32(cmap + 4 + index * 8 + 4);
    const format = view.getUint16(subtable);

    if (format === 12) {
      const groups = view.getUint32(subtable + 12);

      for (let group = 0; group < groups; group++) {
        const at = subtable + 16 + group * 12;
        const start = view.getUint32(at);
        const end = view.getUint32(at + 4);

        if (codePoint >= start && codePoint <= end) {
          return view.getUint32(at + 8) + (codePoint - start);
        }
      }
    }

    if (format === 4) {
      const segments = view.getUint16(subtable + 6) / 2;
      const endCodes = subtable + 14;
      const startCodes = endCodes + segments * 2 + 2;
      const deltas = startCodes + segments * 2;
      const rangeOffsets = deltas + segments * 2;

      for (let segment = 0; segment < segments; segment++) {
        const end = view.getUint16(endCodes + segment * 2);

        if (codePoint > end) {
          continue;
        }

        const start = view.getUint16(startCodes + segment * 2);

        if (codePoint < start) {
          break;
        }

        const delta = view.getInt16(deltas + segment * 2);
        const rangeOffset = view.getUint16(rangeOffsets + segment * 2);

        if (rangeOffset === 0) {
          return (codePoint + delta) & 0xffff;
        }

        const glyph = view.getUint16(
          rangeOffsets + segment * 2 + rangeOffset + (codePoint - start) * 2,
        );

        return glyph === 0 ? 0 : (glyph + delta) & 0xffff;
      }
    }
  }

  return 0;
}

/** Parses the sfnt tables the font check needs. Exported for the unit-free node command only. */
export function readFontRecord(): FontRecord {
  const directories = [join(homedir(), 'Library/Fonts'), '/Library/Fonts'];
  const searched = directories.map((directory) => join(directory, FONT_FILE));
  const path = searched.find((candidate) => existsSync(candidate));

  if (!path) {
    return { found: false, searched, fileName: FONT_FILE };
  }

  const bytes = readFileSync(path);
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const tableCount = view.getUint16(4);
  const tables = new Map<string, number>();

  for (let index = 0; index < tableCount; index++) {
    const entry = 12 + index * 16;
    const tag = String.fromCharCode(...bytes.subarray(entry, entry + 4));

    tables.set(tag, view.getUint32(entry + 8));
  }

  const head = tables.get('head');
  const hhea = tables.get('hhea');
  const hmtx = tables.get('hmtx');
  const cmap = tables.get('cmap');
  const name = tables.get('name');

  if (head === undefined || hhea === undefined || hmtx === undefined || cmap === undefined) {
    return { found: false, searched, fileName: FONT_FILE, path };
  }

  const unitsPerEm = view.getUint16(head + 18);
  const ascender = view.getInt16(hhea + 4);
  const descender = view.getInt16(hhea + 6);
  const metricCount = view.getUint16(hhea + 34);
  const glyph = glyphIdFor(view, cmap, 0x48);
  const advanceH = view.getUint16(hmtx + Math.min(glyph, metricCount - 1) * 4);
  const names = name === undefined ? new Map<number, string>() : nameRecords(view, name);

  return {
    found: true,
    searched,
    fileName: FONT_FILE,
    path,
    sha256: createHash('sha256').update(bytes).digest('hex'),
    family: names.get(1),
    fullName: names.get(4),
    version: names.get(5),
    unitsPerEm,
    ascender,
    descender,
    advanceH,
  };
}
