import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { build } from 'vite';

test('production build rejects an accidentally imported development fixture', async () => {
  const entry = '\0ltac-production-isolation-test';
  const fixtures = fileURLToPath(new URL('../ui/dev/fixtures.ts', import.meta.url)).replaceAll('\\', '/');
  await assert.rejects(build({
    configFile: fileURLToPath(new URL('../vite.config.ts', import.meta.url)),
    logLevel: 'silent',
    build: { write: false, rolldownOptions: { input: entry } },
    plugins: [{
      name: 'accidental-development-import',
      resolveId(id) { if (id === entry) return id; },
      load(id) { if (id === entry) return `import { demoDevices } from ${JSON.stringify(fixtures)}; globalThis.leakedPreview = demoDevices;`; },
    }],
  }), /Development or test modules must not be included/);
});
