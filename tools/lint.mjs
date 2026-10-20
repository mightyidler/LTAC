import { execFileSync } from 'node:child_process';
import { readdirSync } from 'node:fs';

for (const directory of ['tools', 'tests']) {
  for (const file of readdirSync(directory).filter(file => file.endsWith('.mjs'))) {
    execFileSync(process.execPath, ['--check', `${directory}/${file}`], { stdio: 'inherit' });
  }
}
console.log('JavaScript syntax checks passed.');
