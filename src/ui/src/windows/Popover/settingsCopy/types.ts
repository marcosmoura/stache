/** The words for one enum value or one union variant. */
export type OptionCopy = {
  /** Short, sentence case. */
  label: string;
  /** Optional plain-language note, at most 60 characters. */
  hint?: string;
};

/** The words for one setting, section or card, at one copy path. */
export type FieldCopy = {
  /** Short, sentence case (at most 32 characters). */
  label: string;
  /** Optional plain-language sentence (at most 60 characters). */
  hint?: string;
  /** Unit shown beside a number control. */
  unit?: Unit;
  /** Labels of an enum's values, by value. */
  options?: Record<string, OptionCopy>;
  /** Labels of a union's forms, by `variantId`. */
  variants?: Record<string, OptionCopy>;
  /**
   * A table's child keys in display order; keys left out follow in schema
   * order. The feature switch first, then the main settings, then nested
   * sections, with per-layout overrides and advanced settings last.
   */
  order?: readonly string[];
  /**
   * A list's or map's item, singular and capitalised ("Workspace"): titles an
   * untitled item ("Workspace 3") and names the add button ("Add workspace").
   */
  noun?: string;
  /** The label of a union's form menu inside a list item or map entry ("Type"). */
  kindLabel?: string;
  /** The label of a bare value inside a list item or map entry ("Command"). */
  valueLabel?: string;
};

export type Unit = 'px' | 'ms' | 's' | 'minutes' | '%' | '°' | '°C';

/**
 * The copy of one config file, by copy path: keys joined with `.`, with `*`
 * for an array element or a map entry (`workspaces.*.name`).
 */
export type FileCopy = Record<string, FieldCopy>;
