import { defineConfig } from 'vite';
import { readFileSync } from 'node:fs';
const { version } = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8'));
export default defineConfig({
  define: { __APP_VERSION__: JSON.stringify(version) },
  root: 'ui',
  server: { host: '127.0.0.1', port: 1420, strictPort: true },
  build: { outDir: '../web-dist', emptyOutDir: true },
  clearScreen: false,
});
