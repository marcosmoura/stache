import type { ConfigDiagnosticView_Serialize } from '@/ipc';

/** A diagnostic as the parser wrote it: `Line 3, column 7: message (field)`. */
export function diagnosticText(diagnostic: ConfigDiagnosticView_Serialize): string {
  const at =
    diagnostic.line != null
      ? `Line ${diagnostic.line}${diagnostic.column != null ? `, column ${diagnostic.column}` : ''}: `
      : '';
  const field = diagnostic.field ? ` (${diagnostic.field})` : '';

  return `${at}${diagnostic.message}${field}`;
}

/** Common parser and type messages, as a plain-language problem. */
const PROBLEMS: ReadonlyArray<readonly [RegExp, string]> = [
  [/expected `?=`?|missing assignment|key with no value/, 'a key is missing its value'],
  [/expected a key|invalid key|bare key/, 'a key is not written correctly'],
  [/unterminated|unclosed|missing closing quote/, 'some text is missing its closing quote'],
  [/expected `?,`? or `?\]`?|unclosed array/, 'a list is missing its closing bracket'],
  [
    /expected `?,`? or `?\}`?|unclosed inline table/,
    'a group of settings is missing its closing brace',
  ],
  [/duplicate key|defined more than once|duplicate table/, 'the same setting is written twice'],
  [/unknown field/, 'it names a setting Stache does not know'],
  [/unknown variant/, 'a value is not one of the allowed choices'],
  [/missing field/, 'a required setting is missing'],
  [/invalid type/, 'a value has the wrong kind (for example, text instead of a number)'],
  [/invalid value|expected a value|invalid number|invalid string/, 'a value cannot be read'],
];

/** The plain-language problem of a parser or type message, else `null`. */
export function problemOf(message: string): string | null {
  const text = message.toLowerCase();

  return PROBLEMS.find(([pattern]) => pattern.test(text))?.[1] ?? null;
}

/**
 * A diagnostic as a sentence for people: "Line 3 has a problem: a key is
 * missing its value." Unknown messages fall back to "Line 3 can't be read."
 * The raw parser text is never part of it.
 */
export function diagnosticSentence(diagnostic: ConfigDiagnosticView_Serialize): string {
  const problem = problemOf(diagnostic.message);
  const where = diagnostic.line != null ? `Line ${diagnostic.line}` : 'This file';

  return problem === null ? `${where} can't be read.` : `${where} has a problem: ${problem}.`;
}
