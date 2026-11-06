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

test('the production entry defers Three.js until the connection renderer loads', async () => {
  const { output } = await build({ logLevel: 'silent', build: { write: false } });
  const chunks = output.filter(file => file.type === 'chunk');
  const threeChunks = chunks.filter(chunk => Object.keys(chunk.modules).some(id => id.replaceAll('\\', '/').includes('/node_modules/three/')));
  assert.ok(threeChunks.length > 0, 'The connection renderer must still include Three.js');

  const initial = new Set();
  const visit = chunk => {
    if (!chunk || initial.has(chunk.fileName)) return;
    initial.add(chunk.fileName);
    chunk.imports.forEach(name => visit(chunks.find(file => file.fileName === name)));
  };
  chunks.filter(chunk => chunk.isEntry).forEach(visit);
  assert.ok(threeChunks.every(chunk => !initial.has(chunk.fileName)), 'Initial imports must exclude Three.js');
});
