import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { basename, resolve } from 'node:path';

// Run after uploading this signed installer to the matching GitHub Release.
const installer = process.argv[2];
if (!installer?.endsWith('-setup.exe')) throw new Error('Pass the signed NSIS -setup.exe path.');
const config = JSON.parse(await readFile(new URL('../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
if (basename(installer) !== `LTAC_${config.version}_x64-setup.exe`) throw new Error('Installer filename does not match the release version.');
const signature = (await readFile(resolve(`${installer}.sig`), 'utf8')).trim();
if (!signature) throw new Error('Installer signature is empty.');
const manifest = {
  version: config.version,
  notes: `LTAC ${config.version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    'windows-x86_64': {
      signature,
      url: `https://github.com/mightyidler/LTAC/releases/download/v${config.version}/${encodeURIComponent(basename(installer))}`,
    },
  },
};
const output = new URL('../local/release/', import.meta.url);
await mkdir(output, { recursive: true });
await writeFile(new URL('update.json', output), `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`Prepared local/release/update.json for LTAC ${config.version}.`);
