import type { ConfigDiagnosticView_Serialize } from '@/ipc';

/** A diagnostic as one line: `Line 3, column 7: message (field)`. */
export function diagnosticText(diagnostic: ConfigDiagnosticView_Serialize): string {
  const at =
    diagnostic.line != null
      ? `Line ${diagnostic.line}${diagnostic.column != null ? `, column ${diagnostic.column}` : ''}: `
      : '';
  const field = diagnostic.field ? ` (${diagnostic.field})` : '';

  return `${at}${diagnostic.message}${field}`;
}

const TITLE_KEYS = ['name', 'id', 'widget', 'title', 'app_name', 'app_id'];

/** A card's title: a `name`/`id`-like field, a string item, else its position. */
export function itemTitle(item: unknown, index: number): string {
  if (typeof item === 'string' && item !== '') {
    return item;
  }

  if (item && typeof item === 'object' && !Array.isArray(item)) {
    for (const key of TITLE_KEYS) {
      const candidate = (item as Record<string, unknown>)[key];

      if (typeof candidate === 'string' && candidate !== '') {
        return candidate;
      }
    }
  }

  return `Item ${index + 1}`;
}
