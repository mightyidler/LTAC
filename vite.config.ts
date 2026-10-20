import { defineConfig } from 'vite';
import { readFileSync } from 'node:fs';
const { version } = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8'));
export default defineConfig({
  plugins: [{
    name: 'exclude-development-modules',
    generateBundle(_options, bundle) {
      for (const output of Object.values(bundle)) {
        if (output.type !== 'chunk') continue;
        for (const id of Object.keys(output.modules)) {
          if (/\/(?:ui\/dev|tests)\//.test(id.replaceAll('\\', '/'))) {
            this.error('Development or test modules must not be included in production output.');
          }
        }
      }
    },
  }],
  define: { __APP_VERSION__: JSON.stringify(version) },
  root: 'ui',
  publicDir: '../resources/public',
  server: { host: '127.0.0.1', port: 1420, strictPort: true },
  build: {
    outDir: '../web-dist', emptyOutDir: true,
    rolldownOptions: { output: { codeSplitting: {
      groups: [{ name: 'three', test: /node_modules[\\/]three[\\/]/, maxSize: 350_000 }],
    } } },
  },
  clearScreen: false,
});
