// Offline artifact QA matching Minisign's Ed25519 signature and signed comment.
// Runtime installation continues to use Tauri's own signature verifier.
import assert from 'node:assert/strict';
import { createHash, createPublicKey, verify } from 'node:crypto';
import { readFile } from 'node:fs/promises';

const path = process.argv[2];
assert.ok(path?.endsWith('-setup.exe'), 'Pass the signed installer path');
const config = JSON.parse(await readFile(new URL('../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
const publicText = Buffer.from(config.plugins.updater.pubkey, 'base64').toString('utf8');
const publicBytes = Buffer.from(publicText.split(/\r?\n/)[1], 'base64');
const signatureText = Buffer.from((await readFile(`${path}.sig`, 'utf8')).trim(), 'base64').toString('utf8');
const lines = signatureText.split(/\r?\n/);
const signature = Buffer.from(lines[1], 'base64');
assert.equal(publicBytes.length, 42);
assert.equal(signature.length, 74);
assert.deepEqual(signature.subarray(2, 10), publicBytes.subarray(2, 10), 'Release uses the installed updater key');
assert.ok(['Ed', 'ED'].includes(signature.subarray(0, 2).toString()));
assert.ok(lines[2].startsWith('trusted comment: '));
const key = createPublicKey({ key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), publicBytes.subarray(10)]), format: 'der', type: 'spki' });
const data = await readFile(path);
const valid = bytes => verify(null, signature.subarray(0, 2).toString() === 'ED' ? createHash('blake2b512').update(bytes).digest() : bytes, key, signature.subarray(10));
assert.ok(valid(data), 'Installer signature must verify');
assert.ok(verify(null, Buffer.concat([signature.subarray(10), Buffer.from(lines[2].slice(17))]), key, Buffer.from(lines[3], 'base64')), 'Trusted comment signature must verify');
data[Math.floor(data.length / 2)] ^= 1;
assert.equal(valid(data), false, 'A tampered installer must be rejected');
console.log('Installer signature, key identity, signed comment, and tamper rejection passed.');
