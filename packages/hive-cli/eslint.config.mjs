import { defineConfig, globalIgnores } from 'eslint/config';
import { tanstackConfig } from '@tanstack/eslint-config';

export default defineConfig([
  ...tanstackConfig,
  globalIgnores([
    'src/lib/secret-rules.ts', // Large generated file
  ]),
]);
