import js from '@eslint/js';
import pluginQuery from '@tanstack/eslint-plugin-query';
import { defineConfig, globalIgnores } from 'eslint/config';
import { createTypeScriptImportResolver } from 'eslint-import-resolver-typescript';
import { importX } from 'eslint-plugin-import-x';
import reactHooks from 'eslint-plugin-react-hooks';
import reactRefreshPlugin from 'eslint-plugin-react-refresh';
import globals from 'globals';
import { configs as tseslintConfigs } from 'typescript-eslint';

export default defineConfig([
  globalIgnores(['**/dist', 'app/native', 'node_modules', 'target', 'coverage']),
  reactHooks.configs.flat['recommended-latest'],
  ...pluginQuery.configs['flat/recommended'],
  {
    files: ['**/*.{ts,tsx}'],
    extends: [js.configs.recommended, tseslintConfigs.recommended, reactRefreshPlugin.configs.vite],
    languageOptions: {
      ecmaVersion: 'latest',
      globals: {
        ...globals.browser,
        ...globals.es2026,
      },
    },
  },
  {
    files: ['**/*.{ts,tsx}'],
    plugins: {
      'import-x': importX as never,
    },
    extends: [
      'import-x/flat/recommended',
      'import-x/flat/errors',
      'import-x/flat/typescript',
      'import-x/flat/react',
    ],
    settings: {
      'import-x/resolver-next': [
        createTypeScriptImportResolver({
          alwaysTryTypes: true,
          preferRelative: true,
        }),
      ],
    },
    rules: {
      'import-x/first': 'error',
      'import-x/no-duplicates': 'error',
      'import-x/no-dynamic-require': 'error',
      'import-x/order': [
        'error',
        {
          groups: [
            'builtin', // Node.js built-in modules
            'external', // npm packages
            'internal', // local packages (workspace packages)
            'parent', // relative imports from parent directories
            [
              'sibling', // relative imports from same directory
              'index', // index file imports
            ],
            'object', // imports of objects
          ],
          'newlines-between': 'always',
          distinctGroup: true,
          alphabetize: {
            order: 'asc',
            caseInsensitive: true,
          },
          pathGroups: [
            {
              pattern: '{react,react/**,react-dom,react-dom/**,react-*,react-*/**}',
              group: 'external',
              position: 'before',
            },
            {
              pattern: '@/**',
              group: 'internal',
              position: 'before',
            },
            {
              pattern: './*.*',
              group: 'index',
              position: 'after',
            },
          ],
          pathGroupsExcludedImportTypes: ['builtin'],
        },
      ],
    },
  },
]);
