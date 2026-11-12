import { test as base, expect } from '@playwright/test';

import { FakeHost } from './fakeHost';

type Fixtures = {
  /** The scripted fake host, installed before the page loads. */
  host: FakeHost;
  /** Console errors and uncaught page errors seen during the test. */
  consoleErrors: string[];
};

export const test = base.extend<Fixtures>({
  consoleErrors: async ({ page }, provide) => {
    const errors: string[] = [];

    page.on('console', (message) => {
      if (message.type() === 'error') {
        errors.push(message.text());
      }
    });
    page.on('pageerror', (error) => {
      errors.push(error.message);
    });
    await provide(errors);
  },
  host: async ({ page }, provide) => {
    await provide(await FakeHost.install(page));
  },
});

export { expect };
