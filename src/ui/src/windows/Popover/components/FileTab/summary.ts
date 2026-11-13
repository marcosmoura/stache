/**
 * The short help shown under a setting: the first sentence of its schema
 * description. The full text stays available as a tooltip and to assistive
 * technology.
 */
export function summary(description: string): string {
  const firstLine = description.trim().split('\n')[0]?.trim() ?? '';
  const sentence = /^.*?[.!?](?=\s|$)/.exec(firstLine);

  return sentence ? sentence[0] : firstLine;
}
