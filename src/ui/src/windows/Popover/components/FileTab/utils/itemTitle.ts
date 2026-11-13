import type { FieldCopy } from '@/windows/Popover/settingsCopy';

const TITLE_KEYS = ['name', 'title', 'app_name', 'app_id'];

/**
 * A card's title, from the copy of the list's item (`copy`, at `list.*`) and
 * the list's noun: a name or title field; else the human name of a widget
 * (a string item or a `widget` tag); else an `id`; else "Workspace 3".
 */
export function itemTitle(
  item: unknown,
  index: number,
  copy: Pick<FieldCopy, 'options' | 'variants'>,
  noun: string,
): string {
  const named = (value: unknown) =>
    typeof value === 'string' && value !== ''
      ? (copy.options?.[value] ?? copy.variants?.[value])?.label
      : undefined;

  if (typeof item === 'string' && item !== '') {
    return named(item) ?? item;
  }

  if (item && typeof item === 'object' && !Array.isArray(item)) {
    const table = item as Record<string, unknown>;

    for (const key of TITLE_KEYS) {
      const candidate = table[key];

      if (typeof candidate === 'string' && candidate !== '') {
        return candidate;
      }
    }

    const title = named(table.widget) ?? table.id;

    if (typeof title === 'string' && title !== '') {
      return title;
    }
  }

  return `${noun} ${index + 1}`;
}
