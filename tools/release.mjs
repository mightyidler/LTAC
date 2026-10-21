import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const run = (command, args, options = {}) => execFileSync(command, args, { cwd: root, stdio: 'inherit', ...options });
const git = (...args) => run('git', args, { stdio: 'pipe' }).toString().trim();
const npm = script => run(process.execPath, [process.env.npm_execpath, 'run', script]);
const read = path => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const write = (path, value) => writeFileSync(new URL(`../${path}`, import.meta.url), value);
const version = process.argv[2]?.replace(/^v/, '');
const current = JSON.parse(read('package.json')).version;
if (!/^\d+\.\d+\.\d+$/.test(version ?? '')) throw new Error('Usage: npm run release -- 0.1.7');
const parts = version.split('.').map(Number), old = current.split('.').map(Number);
const difference = parts.findIndex((value, index) => value !== old[index]);
if (difference < 0 || parts[difference] < old[difference]) throw new Error('Choose a version newer than package.json.');
if (git('branch', '--show-current') !== 'main') throw new Error('Release from main.');
if (git('status', '--porcelain')) throw new Error('Commit your changes before running the release command.');
run('git', ['fetch', 'origin', '--tags']);
// Integrate remote automation commits before changing version files. Local-only
// commits are replayed on the updated main; conflicts stop before a tag exists.
run('git', ['rebase', 'origin/main']);
if (git('tag', '--list', `v${version}`)) throw new Error(`v${version} already exists; choose a new version.`);
// Re-read after fast-forwarding; the remote may already contain a newer release.
if (JSON.parse(read('package.json')).version !== current) throw new Error('Remote version changed. Run the command again with the next version.');

const paths = ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json', 'Cargo.toml', 'src-tauri/Cargo.toml', 'Cargo.lock', 'src-tauri/Cargo.lock'];
const originals = new Map(paths.map(path => [path, read(path)]));
for (const path of paths) {
  let content = read(path);
  if (path.endsWith('.json')) {
    const data = JSON.parse(content);
    data.version = version;
    if (data.packages?.['']) data.packages[''].version = version;
    content = `${JSON.stringify(data, null, 2)}\n`;
  } else if (path.endsWith('.lock')) {
    content = content.replace(/(\[\[package\]\]\r?\nname = "ltac(?:-desktop)?"\r?\nversion = ")[^"]+"/g, `$1${version}"`);
  } else {
    content = content.replace(/^version = "[^"]+"/m, `version = "${version}"`);
  }
  write(path, content);
}
console.log(`Checking LTAC ${version} before creating a release commit or tag...`);
try {
  npm('verify:release');
} catch (error) {
  for (const [path, content] of originals) write(path, content);
  throw error;
}
run('git', ['add', '--', ...paths]);
run('git', ['commit', '-m', `release: LTAC v${version}`]);
run('git', ['tag', `v${version}`]);
// Publish the commit and its tag together; either both refs advance or neither does.
run('git', ['push', '--atomic', 'origin', 'main', `v${version}`], {
  env: { ...process.env, LTAC_RELEASE_VERIFIED: '1' },
});
console.log(`Pushed v${version}. Follow Release LTAC in GitHub Actions for installer verification and publication.`);
