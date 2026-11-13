import { describe, expect, it } from 'vitest';

import { diagnosticText, itemTitle } from './FileTab.model';
import {
  getAt,
  humanise,
  insertOp,
  moveOp,
  pathText,
  removeOp,
  renameOp,
  setAt,
  setOp,
  tomlText,
} from './values';

const FILE = { gaps: { inner: 4 }, workspaces: [{ name: 'a' }, { name: 'b' }] };
const DEFAULTS = { gaps: { inner: 0, outer: 0 }, workspaces: [], layout: { start: ['clock'] } };

describe('values', () => {
  it('reads and writes at key paths', () => {
    expect(getAt(FILE, ['workspaces', 1, 'name'])).toBe('b');
    expect(getAt(FILE, ['workspaces', 5])).toBeUndefined();
    expect(getAt(FILE, ['gaps', 'inner', 'x'])).toBeUndefined();
    expect(setAt(FILE, ['gaps', 'outer'], 2)).toEqual({
      ...FILE,
      gaps: { inner: 4, outer: 2 },
    });
    expect(setAt([1, 2, 3], [1], undefined)).toEqual([1, 3]);
  });

  it('plans direct ops when the target is in the file', () => {
    expect(setOp(FILE, DEFAULTS, ['gaps', 'outer'], 3)).toEqual({
      op: 'set',
      path: ['gaps', 'outer'],
      value: 3,
    });
    expect(setOp(FILE, DEFAULTS, ['gaps', 'inner'], null)).toEqual({
      op: 'remove',
      path: ['gaps', 'inner'],
    });
    expect(removeOp(FILE, DEFAULTS, ['workspaces', 0])).toEqual({
      op: 'remove',
      path: ['workspaces', 0],
    });
    expect(insertOp(FILE, DEFAULTS, ['workspaces'], 2, { name: 'c' })).toEqual({
      op: 'insert',
      path: ['workspaces'],
      index: 2,
      value: { name: 'c' },
    });
    expect(moveOp(FILE, DEFAULTS, ['workspaces'], 0, 1)).toEqual({
      op: 'move',
      path: ['workspaces'],
      from: 0,
      to: 1,
    });
    expect(renameOp({ bindings: { a: 'x' } }, {}, ['bindings'], 'a', 'b')).toEqual({
      op: 'rename',
      path: ['bindings'],
      from: 'a',
      to: 'b',
    });
  });

  it('materialises an array that only exists as a default', () => {
    expect(setOp({}, DEFAULTS, ['layout', 'start', 0], 'cpu')).toEqual({
      op: 'set',
      path: ['layout', 'start'],
      value: ['cpu'],
    });
    expect(insertOp({}, DEFAULTS, ['layout', 'start'], 1, 'wifi')).toEqual({
      op: 'set',
      path: ['layout', 'start'],
      value: ['clock', 'wifi'],
    });
    expect(removeOp({}, DEFAULTS, ['layout', 'start', 0])).toEqual({
      op: 'set',
      path: ['layout', 'start'],
      value: [],
    });
    expect(moveOp({}, { list: [1, 2] }, ['list'], 0, 1)).toEqual({
      op: 'set',
      path: ['list'],
      value: [2, 1],
    });
    expect(renameOp({}, { bindings: { a: 'x' } }, ['bindings'], 'a', 'b')).toEqual({
      op: 'set',
      path: ['bindings', 'b'],
      value: 'x',
    });
  });

  it('formats paths, labels, values, diagnostics and card titles', () => {
    expect(pathText(['workspaces', 1, 'layout'])).toBe('workspaces[1].layout');
    expect(humanise('default_layout')).toBe('Default layout');
    expect(humanise('hourCycle')).toBe('Hour cycle');
    expect(tomlText({ a: 1, 'b c': ['x', true] })).toBe('{ a = 1, "b c" = ["x", true] }');
    expect(diagnosticText({ message: 'expected `=`', line: 3, column: 7, field: null })).toBe(
      'Line 3, column 7: expected `=`',
    );
    expect(diagnosticText({ message: 'bad', line: null, column: null, field: 'gaps' })).toBe(
      'bad (gaps)',
    );
    expect(itemTitle({ name: 'coding' }, 0)).toBe('coding');
    expect(itemTitle({ widget: 'cpu', id: 'c1' }, 0)).toBe('c1');
    expect(itemTitle('clock', 0)).toBe('clock');
    expect(itemTitle({}, 2)).toBe('Item 3');
  });
});
