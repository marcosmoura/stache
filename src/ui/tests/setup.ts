import 'vitest-browser-react';
import { beforeEach } from 'vitest';

beforeEach(() => {
  document.body.dataset.testSetup = 'ready';
});
