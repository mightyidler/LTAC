import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';

test('release command rolls back failed validation and atomically publishes matching versions and tag', () => {
  const directory = mkdtempSync(join(tmpdir(), 'ltac-release-test-'));
  const cwd = join(directory, 'work');
  const git = (...args) => execFileSync('git', args, { cwd, stdio: 'pipe' }).toString().trim();
  const read = path => readFileSync(join(cwd, path), 'utf8');
  const write = (path, content) => writeFileSync(join(cwd, path), content);
  try {
    mkdirSync(join(cwd, 'tools'), { recursive: true });
    mkdirSync(join(cwd, 'src-tauri'));
    execFileSync('git', ['init', '--bare', join(directory, 'remote.git')], { stdio: 'pipe' });
    git('init', '-b', 'main');
    git('config', 'user.name', 'Release test');
    git('config', 'user.email', 'test@example.invalid');
    copyFileSync(new URL('../tools/release.mjs', import.meta.url), join(cwd, 'tools/release.mjs'));
    write('package.json', JSON.stringify({ name: 'ltac', version: '0.1.6', type: 'module' }));
    write('package-lock.json', JSON.stringify({ version: '0.1.6', packages: { '': { version: '0.1.6' }, dependency: { version: '0.1.6' } } }));
    write('src-tauri/tauri.conf.json', JSON.stringify({ version: '0.1.6' }));
    write('Cargo.toml', '[package]\nname = "ltac"\nversion = "0.1.6"\n');
    write('src-tauri/Cargo.toml', '[package]\nname = "ltac-desktop"\nversion = "0.1.6"\n');
    const lock = ['ltac', 'ltac-desktop', 'dependency'].map(name => `[[package]]\nname = "${name}"\nversion = "0.1.6"\n`).join('\n');
    write('Cargo.lock', lock);
    write('src-tauri/Cargo.lock', lock);
    git('add', '.');
    git('commit', '-m', 'initial');
    git('remote', 'add', 'origin', join(directory, 'remote.git'));
    git('push', '-u', 'origin', 'main');
    // Substitute only the expensive validation process; all Git and file edits are real.
    const validation = join(directory, 'validate.cjs');
    writeFileSync(validation, "if (process.argv.slice(2).join(' ') !== 'run verify:release') process.exit(2); process.exit(process.env.FAIL_VALIDATION ? 1 : 0);\n");
    const invoke = extra => spawnSync(process.execPath, ['tools/release.mjs', '0.1.7'], {
      cwd, encoding: 'utf8', env: { ...process.env, npm_execpath: validation, ...extra },
    });
    const originalHead = git('rev-parse', 'HEAD');
    assert.notEqual(invoke({ FAIL_VALIDATION: '1' }).status, 0);
    assert.equal(git('status', '--porcelain'), '', 'Validation failure restores exact original files');
    assert.equal(git('rev-parse', 'HEAD'), originalHead);
    assert.equal(git('tag', '--list'), '', 'Validation failure creates no tag');
    // Reproduce the original fetch-first cause: a remote bot commit and a local
    // feature commit made since the previous release must both survive.
    const remoteWork = join(directory, 'remote-work');
    execFileSync('git', ['clone', '-b', 'main', join(directory, 'remote.git'), remoteWork], { stdio: 'pipe' });
    const remoteGit = (...args) => execFileSync('git', args, { cwd: remoteWork, stdio: 'pipe' });
    remoteGit('config', 'user.name', 'Remote test');
    remoteGit('config', 'user.email', 'remote@example.invalid');
    writeFileSync(join(remoteWork, 'update.json'), '{"version":"0.1.6"}\n');
    remoteGit('add', 'update.json');
    remoteGit('commit', '-m', 'remote manifest');
    remoteGit('push', 'origin', 'main');
    write('feature.txt', 'local feature\n');
    git('add', 'feature.txt');
    git('commit', '-m', 'local feature');
    const result = invoke({ FAIL_VALIDATION: '' });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(read('feature.txt').trim(), 'local feature');
    assert.equal(JSON.parse(read('update.json')).version, '0.1.6');
    for (const path of ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json']) assert.equal(JSON.parse(read(path)).version, '0.1.7');
    assert.equal(JSON.parse(read('package-lock.json')).packages[''].version, '0.1.7');
    assert.equal(JSON.parse(read('package-lock.json')).packages.dependency.version, '0.1.6');
    for (const path of ['Cargo.lock', 'src-tauri/Cargo.lock']) {
      assert.match(read(path), /name = "ltac"\nversion = "0.1.7"/);
      assert.match(read(path), /name = "ltac-desktop"\nversion = "0.1.7"/);
      assert.match(read(path), /name = "dependency"\nversion = "0.1.6"/);
    }
    assert.equal(git('rev-parse', 'HEAD'), git('rev-parse', 'v0.1.7'));
    const refs = git('ls-remote', 'origin', 'refs/heads/main', 'refs/tags/v0.1.7').split('\n');
    assert.equal(refs.length, 2);
    assert(refs.every(line => line.startsWith(git('rev-parse', 'HEAD'))));
    assert.equal(git('status', '--porcelain'), '');
  } finally {
    assert.equal(dirname(resolve(directory)), resolve(tmpdir()));
    rmSync(directory, { recursive: true, force: true });
  }
});

test('manifest generation preserves the legacy endpoint and writes the signed release asset separately', () => {
  const directory = mkdtempSync(join(tmpdir(), 'ltac-manifest-test-'));
  try {
    mkdirSync(join(directory, 'tools'));
    mkdirSync(join(directory, 'src-tauri'));
    copyFileSync(new URL('../tools/publish-update.mjs', import.meta.url), join(directory, 'tools/publish-update.mjs'));
    writeFileSync(join(directory, 'src-tauri/tauri.conf.json'), JSON.stringify({ version: '0.1.7' }));
    const legacy = '{"version":"0.1.6","release_asset_migration":true}\n';
    writeFileSync(join(directory, 'update.json'), legacy);
    const installer = join(directory, 'LTAC_0.1.7_x64-setup.exe');
    writeFileSync(`${installer}.sig`, 'test-signature\n');
    execFileSync(process.execPath, ['tools/publish-update.mjs', installer], { cwd: directory, stdio: 'pipe' });
    assert.equal(readFileSync(join(directory, 'update.json'), 'utf8'), legacy);
    const manifest = JSON.parse(readFileSync(join(directory, 'local/release/update.json'), 'utf8'));
    assert.equal(manifest.version, '0.1.7');
    assert.equal(manifest.platforms['windows-x86_64'].signature, 'test-signature');
    assert.equal(manifest.platforms['windows-x86_64'].url, 'https://github.com/mightyidler/LTAC/releases/download/v0.1.7/LTAC_0.1.7_x64-setup.exe');
  } finally {
    assert.equal(dirname(resolve(directory)), resolve(tmpdir()));
    rmSync(directory, { recursive: true, force: true });
  }
});
