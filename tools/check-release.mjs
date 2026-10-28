import assert from 'node:assert/strict';
import { readFile, access } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const read = path => readFile(new URL(`../${path}`, import.meta.url), 'utf8');
const pkg = JSON.parse(await read('package.json'));
const lock = JSON.parse(await read('package-lock.json'));
const config = JSON.parse(await read('src-tauri/tauri.conf.json'));
const version = pkg.version;
assert.equal(config.version, version);
assert.equal(lock.version, version);
assert.equal(lock.packages[''].version, version);
for (const path of ['Cargo.toml', 'src-tauri/Cargo.toml']) {
  assert.equal((await read(path)).match(/^version = "([^"]+)"/m)[1], version, path);
}
assert.equal(config.bundle.windows.nsis.installMode, 'perMachine');
assert.equal(config.bundle.createUpdaterArtifacts, true);
assert.equal(config.productName, 'LTAC');
assert.equal(config.bundle.publisher, 'LTAC');
for (const window of config.app.windows) assert.equal(window.title, 'LTAC');
assert.equal(config.bundle.resources['../target/release/ltac-service.exe'], 'ltac-service.exe');
if (process.argv.includes('--package')) {
  for (const path of Object.keys(config.bundle.resources)) {
    await access(new URL(`../src-tauri/${path}`, import.meta.url));
  }
  const { readdir } = await import('node:fs/promises');
  const directory = new URL('../src-tauri/target/release/bundle/nsis/', import.meta.url);
  const installers = (await readdir(directory)).filter(name => name.endsWith('-setup.exe') && name.includes(`_${version}_`));
  assert.equal(installers.length, 1);
  assert.ok(installers[0].includes(`_${version}_`));
  assert.ok((await readFile(new URL(`${installers[0]}.sig`, directory), 'utf8')).trim());
  // Extract without installing or executing anything. Compare every bundled
  // resource with its exact source, so a stale service cannot ship unnoticed.
  const output = await mkdtemp(join(tmpdir(), 'ltac-package-'));
  let sevenZip = '7z.exe';
  try { await access('C:/Program Files/7-Zip/7z.exe'); sevenZip = 'C:/Program Files/7-Zip/7z.exe'; } catch {}
  const { fileURLToPath } = await import('node:url');
  execFileSync(sevenZip, ['x', fileURLToPath(new URL(installers[0], directory)), `-o${output}`, '-y'], { stdio: 'pipe' });
  assert.deepEqual(await readFile(join(output, 'LTAC.exe')), await readFile(new URL('../src-tauri/target/release/LTAC.exe', import.meta.url)), 'LTAC.exe must match the release build');
  for (const [source, destination] of Object.entries(config.bundle.resources)) {
    assert.deepEqual(await readFile(join(output, destination)), await readFile(new URL(`../src-tauri/${source}`, import.meta.url)), `${destination} must match the release build`);
  }
  console.log('NSIS extraction: LTAC.exe, service, CLI, and installer script match the release sources.');
}
console.log(`LTAC ${version}: version consistency and installer resources passed.`);
