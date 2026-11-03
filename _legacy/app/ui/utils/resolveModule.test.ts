import { describe, expect, it } from 'vitest';

import { resolveModule } from './resolveModule';

describe('resolveModule', () => {
  it('should export the resolveModule function', () => {
    expect(resolveModule).toBeDefined();
    expect(typeof resolveModule).toBe('function');
  });

  it('should return a function when called with a module name', () => {
    const resolver = resolveModule('TestComponent');
    expect(typeof resolver).toBe('function');
  });

  it('should extract the named export and wrap it as default export', () => {
    const MockComponent = () => null;
    const module = { TestComponent: MockComponent };

    const resolver = resolveModule('TestComponent');
    const result = resolver(module);

    expect(result).toHaveProperty('default');
    expect(result.default).toBe(MockComponent);
  });

  it('should work with different module names', () => {
    const ComponentA = () => null;
    const ComponentB = () => null;
    const module = { ComponentA, ComponentB };

    const resolverA = resolveModule('ComponentA');
    const resolverB = resolveModule('ComponentB');

    expect(resolverA(module).default).toBe(ComponentA);
    expect(resolverB(module).default).toBe(ComponentB);
  });

  it('should return undefined for non-existent exports', () => {
    const module = { ExistingComponent: () => null };

    const resolver = resolveModule('NonExistentComponent');
    const result = resolver(module);

    expect(result.default).toBeUndefined();
  });

  it('should be usable in a Promise.then chain pattern', async () => {
    const MockComponent = () => null;
    const mockImport = Promise.resolve({ Bar: MockComponent });

    const result = await mockImport.then(resolveModule('Bar'));

    expect(result.default).toBe(MockComponent);
  });
});
