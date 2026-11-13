import { describe, expect, it } from 'vitest';

import type { ConfigDocument_Serialize, JsonValue } from '@/ipc';

import { flush, mockHost } from '../../../../../tests/ipc';
import { render } from '../../../../../tests/render';

import { FileTab } from './FileTab';

/*
 * The visible text of every file tab, with realistic data and every section
 * and card expanded, against a deny-list of machine text: schema keys,
 * backticks, schema descriptions, quoted JSON and "Item 3".
 */

const SCHEMAS = import.meta.glob<JsonValue>('../../../../../../../schemas/*.json', {
  eager: true,
  import: 'default',
});

const schema = (name: string) =>
  Object.entries(SCHEMAS).find(([path]) => path.endsWith(`/${name}.schema.json`))![1];

const rule = { app_name: 'Finder' };

/** Realistic files, with every list and map holding items. */
const FILES: Record<string, JsonValue> = {
  global: {
    exec_on_startup: ['open -a Ghostty', 'echo ready'],
    logging: { level: 'info', debug: false },
    ui: { appearance: 'system' },
  },
  bar: {
    enabled: true,
    height: 32,
    layout: {
      start: ['spaces'],
      center: ['media'],
      end: ['weather', { widget: 'cpu', temperature_alert: 90 }, 'keep_awake', { widget: 'clock' }],
    },
    weather: { provider: 'open_meteo', default_location: 'Lisbon' },
  },
  commandQuit: { enabled: true, hold_duration: 600 },
  keepAwake: { enabled: true },
  keybindings: {
    enabled: true,
    bindings: { 'cmd+shift+t': 'open -a Ghostty', 'cmd+shift+b': ['open -a Safari', 'echo done'] },
  },
  menuAnywhere: { enabled: true, modifiers: ['control', 'option'], mouse_button: 'right_click' },
  notunes: { enabled: true, target_app: 'Spotify' },
  proxyAudio: {
    enabled: true,
    input: [{ name: 'MacBook Pro Microphone', strategy: 'exact' }],
    output: [
      { name: 'AirPods', strategy: 'contains', depends_on: { name: 'Dock', strategy: 'exact' } },
    ],
  },
  tiling: {
    enabled: true,
    default_layout: 'dwindle',
    gaps: { inner: 8, outer: { horizontal: 12, vertical: 10 } },
    master: { ratio: 60 },
    workspaces: [
      { name: 'coding', layout: 'master', rules: [rule, { title: 'Notes' }] },
      { name: 'browser', screen: 'secondary' },
      {},
    ],
    ignore: [rule, {}],
    floating: { presets: [{ name: 'half', width: '50%', height: 600 }] },
    borders: {
      enabled: true,
      focused: { color: '#89b4fa', width: 4 },
      unfocused: { glow: '#000000', width: 2 },
      floating: { gradient: { from: '#89b4fa', to: '#cba6f7', angle: 90 }, width: 4 },
      ignore: [rule],
    },
  },
  wallpapers: { enabled: true, list: ['beach.jpg', 'forest.jpg'], mode: 'random' },
};

function doc(name: string): ConfigDocument_Serialize {
  return {
    file: `${name}.toml`,
    path: `/Users/me/.config/stache/${name}.toml`,
    state: 'valid',
    hash: 'h1',
    value: FILES[name],
    default: {},
    schema: schema(name),
    diagnostic: null,
    warnings: [],
  };
}

/** Every `description` in a schema. */
function descriptions(value: unknown, out: string[] = []): string[] {
  if (Array.isArray(value)) {
    value.forEach((item) => descriptions(item, out));
  } else if (value && typeof value === 'object') {
    for (const [key, item] of Object.entries(value)) {
      if (key === 'description' && typeof item === 'string') {
        out.push(item);
      } else {
        descriptions(item, out);
      }
    }
  }

  return out;
}

/** Opens every collapsed section and card until none is left. */
async function expandAll() {
  for (let round = 0; round < 12; round += 1) {
    const closed = [
      ...document.querySelectorAll<HTMLButtonElement>(
        '[data-testid="settings-form"] button[aria-expanded="false"]:not([aria-haspopup])',
      ),
    ];

    if (closed.length === 0) {
      return;
    }

    closed.forEach((button) => button.click());
    await flush();
  }
}

describe('FileTab visible text', () => {
  for (const name of Object.keys(FILES)) {
    it(`${name}.toml reads like System Settings`, async () => {
      mockHost().answer('config_document', () => ({ status: 'document', document: doc(name) }));
      const screen = await render(<FileTab file={`${name}.toml`} />);

      await expect.element(screen.getByTestId('settings-form')).toBeVisible();
      await expandAll();

      const form = document.querySelector<HTMLElement>('[data-testid="settings-form"]')!;
      const text = form.innerText;
      const found: string[] = [];

      for (const description of descriptions(schema(name))) {
        if (text.includes(description.split('\n')[0])) {
          found.push(`description: ${description.slice(0, 40)}`);
        }
      }

      const deny: Array<[string, RegExp]> = [
        ['snake_case key', /\b[a-z]+_[a-z0-9_]+\b/],
        ['backtick', /`/],
        ['quoted JSON', /"[^"\n]*"|[{}[\]]/],
        ['Item N', /\bItem \d/],
      ];

      for (const [what, pattern] of deny) {
        const match = pattern.exec(text);

        if (match) {
          found.push(`${what}: ${match[0]}`);
        }
      }

      expect(text.length).toBeGreaterThan(0);
      expect(found).toEqual([]);
    });
  }
});
