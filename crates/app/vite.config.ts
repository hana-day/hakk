import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// The web UI lives in `ui/`; Tauri serves the build output from `dist/` (see tauri.conf.json).
export default defineConfig({
  root: 'ui',
  plugins: [svelte({ configFile: '../svelte.config.js' })],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { outDir: '../dist', emptyOutDir: true, target: 'es2022' },
  test: { include: ['src/**/*.test.ts'] },
});
