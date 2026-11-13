import { describe, expect, it } from 'vitest';

import { diagnosticSentence, diagnosticText, problemOf } from './diagnostic';

const at = (message: string, line: number | null = 3) => ({
  message,
  line,
  column: line === null ? null : 7,
  field: null,
});

describe('diagnostic', () => {
  it('keeps the raw text for the details', () => {
    expect(diagnosticText(at('expected `=`'))).toBe('Line 3, column 7: expected `=`');
    expect(diagnosticText({ message: 'bad', line: null, column: null, field: 'gaps' })).toBe(
      'bad (gaps)',
    );
  });

  it('turns common parser and type messages into a sentence with the line', () => {
    expect(diagnosticSentence(at('expected `=` after a key'))).toBe(
      'Line 3 has a problem: a key is missing its value.',
    );
    expect(diagnosticSentence(at('unterminated string'))).toBe(
      'Line 3 has a problem: some text is missing its closing quote.',
    );
    expect(diagnosticSentence(at('expected `,` or `]`'))).toBe(
      'Line 3 has a problem: a list is missing its closing bracket.',
    );
    expect(diagnosticSentence(at('unknown variant `fuzzy`, expected one of `a`, `b`'))).toBe(
      'Line 3 has a problem: a value is not one of the allowed choices.',
    );
    expect(diagnosticSentence(at('invalid type: string "x", expected u32', null))).toBe(
      'This file has a problem: a value has the wrong kind (for example, text instead of a number).',
    );
  });

  it('falls back to a generic sentence, never the raw message', () => {
    expect(diagnosticSentence(at('frobnicated `zork` at byte 9'))).toBe("Line 3 can't be read.");
    expect(diagnosticSentence(at('???', null))).toBe("This file can't be read.");
    expect(problemOf('frobnicated')).toBeNull();
  });

  it('never shows code punctuation', () => {
    for (const message of [
      'expected `=`',
      'invalid key',
      'duplicate key `a`',
      'missing field `b`',
    ]) {
      expect(diagnosticSentence(at(message))).not.toMatch(/[`{}[\]=]/);
    }
  });
});
