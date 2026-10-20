import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { checkRepository, inspectFile } from '../tools/check-repository.mjs';

test('credentials are detected in ordinary source, including test files', () => {
  const token = 'ghp_' + 'A'.repeat(36);
  for (const path of ['ui/src/example.ts', 'tests/example.mjs']) {
    const findings = inspectFile(path, Buffer.from(`const token = '${token}';`));
    assert.equal(findings[0].rule, 'GitHub token');
    assert(!JSON.stringify(findings).includes(token), 'Diagnostics must not disclose a credential');
  }
  assert.equal(inspectFile('src/example.rs', Buffer.from('const config = {"client-key":"' + 'unexpected-credential' + '"};'))[0].rule, 'literal credential');
  const header = '-----BEGIN ' + 'PRIVATE KEY-----';
  assert.equal(inspectFile('notes.md', Buffer.from(header))[0].rule, 'private key');
});

test('generated/private paths are blocked even for binary or forced additions', () => {
  for (const path of ['local/settings.json', '.env.production', 'nested/signing.key', 'docs/VALIDATION.md', 'config.json', 'deliverables/app.exe', 'nested/WebView2/state']) {
    assert.equal(inspectFile(path, Buffer.from([0, 1]))[0].rule, 'private, generated or internal-only file', path);
  }
  for (const path of ['Cargo.lock', 'src-tauri/Cargo.lock', 'resources/public/fonts/pretendard/OFL.txt', 'docs/DEVELOPMENT.md']) {
    assert.deepEqual(inspectFile(path, Buffer.from('safe')), [], path);
  }
});

test('staged scanning reads the index even when the working file is clean', () => {
  const cwd = mkdtempSync(join(tmpdir(), 'ltac-repository-test-'));
  try {
    execFileSync('git', ['init', '--quiet'], { cwd });
    const token = 'ghp_' + 'B'.repeat(36);
    writeFileSync(join(cwd, 'example.txt'), token);
    execFileSync('git', ['add', 'example.txt'], { cwd });
    writeFileSync(join(cwd, 'example.txt'), 'safe');
    assert.equal(checkRepository({ cwd, staged: true }).findings[0].rule, 'GitHub token');
    assert.deepEqual(checkRepository({ cwd }).findings, []);
    writeFileSync(join(cwd, '.env'), 'safe');
    execFileSync('git', ['add', '-f', '.env'], { cwd });
    assert(checkRepository({ cwd, staged: true }).findings.some(f => f.path === '.env'));
  } finally {
    assert.equal(dirname(resolve(cwd)), resolve(tmpdir()), 'Only remove this test temporary directory');
    rmSync(cwd, { recursive: true, force: true });
  }
});
