import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, realpathSync } from 'node:fs';
import { isAbsolute, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const blockedPath = /(^|\/)(?:\.env(?:\..*)?|\.secrets|\.credentials|node_modules|target|web-dist|deliverables|releases|local|coverage|test-results|playwright-report|WebView2|\.idea|\.vscode)(?:\/|$)|(^|\/)(?:id_rsa|id_ed25519|config(?:\.[^/]*)?\.json|service-config\.json)$|\.(?:env(?:\..*)?|pem|key|p12|pfx|secret|secrets\.[^/]*|exe|dll|msi|zip|7z|sig|log|jsonl|tmp|bak|dmp|tsbuildinfo)$/i;
const publicDocs = new Set(['docs/DEVELOPMENT.md', 'docs/media/ltac-demo.gif']);
const secretPatterns = [
  ['private key', /-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----/g],
  ['GitHub token', /\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{50,})\b/g],
  ['AWS access key', /\b(?:AKIA|ASIA)[A-Z0-9]{16}\b/g],
  ['API token', /\b(?:sk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{32,}|xox[baprs]-[A-Za-z0-9-]{20,}|AIza[A-Za-z0-9_-]{35})\b/g],
  ['credential in URL', /https?:\/\/[^\s\/@:]+:[^\s\/@]+@/g],
  ['minisign secret key', /untrusted comment: minisign (?:encrypted )?secret key/gi],
];

// Report locations and rule names only. Never emit matched credential values.
export function inspectFile(path, content, { history = false } = {}) {
  const findings = [];
  if (!history && (blockedPath.test(path) || (path.startsWith('docs/') && !publicDocs.has(path)))) {
    findings.push({ path, rule: 'private, generated or internal-only file' });
  }
  if (content.includes(0)) return findings;
  const source = content.toString('utf8');
  const report = (index, rule) => findings.push({ path, line: source.slice(0, index).split('\n').length, rule });
  for (const [rule, pattern] of secretPatterns) {
    for (const match of source.matchAll(pattern)) report(match.index, rule);
  }
  const literal = /["']?(?:client[-_]key|api[-_]key|access[-_]token|refresh[-_]token|password|private[-_]key|aws_secret_access_key)["']?\s*[:=]\s*["']([^"'\r\n]{8,})["']/gi;
  for (const match of source.matchAll(literal)) {
    const inRustTests = path.endsWith('.rs') && source.slice(0, match.index).includes('#[cfg(test)]');
    const knownTestKey = inRustTests && ['test-key', 'saved-key'].includes(match[1]);
    if (!knownTestKey) report(match.index, 'literal credential');
  }
  return findings;
}

export function checkRepository({ cwd = process.cwd(), staged = false, history = false } = {}) {
  const git = (args, options = {}) => execFileSync('git', args, { cwd, maxBuffer: 128 * 1024 * 1024, ...options });
  const findings = [];
  let checked = 0;
  if (history) {
    const objects = git(['rev-list', '--objects', '--all']).toString().trim().split('\n').flatMap(line => {
      const split = line.indexOf(' ');
      return split < 0 ? [] : [{ oid: line.slice(0, split), path: line.slice(split + 1) }];
    });
    const batch = git(['cat-file', '--batch'], { input: objects.map(o => o.oid).join('\n') + '\n' });
    let offset = 0;
    for (const object of objects) {
      const end = batch.indexOf(10, offset);
      const [, type, sizeText] = batch.subarray(offset, end).toString().split(' ');
      const size = Number(sizeText);
      offset = end + 1;
      const content = batch.subarray(offset, offset + size);
      offset += size + 1;
      if (type !== 'blob') continue;
      checked++;
      findings.push(...inspectFile(object.path, content, { history: true }).map(f => ({ ...f, blob: object.oid.slice(0, 12) })));
    }
  } else {
    const args = staged ? ['diff', '--cached', '--name-only', '--diff-filter=ACMR', '-z'] : ['ls-files', '-z'];
    const paths = git(args).toString().split('\0').filter(Boolean);
    if (!staged) paths.push(...git(['ls-files', '--others', '--exclude-standard', '-z']).toString().split('\0').filter(Boolean));
    const root = realpathSync(cwd);
    for (const path of new Set(paths)) {
      const absolute = resolve(cwd, path);
      if (!staged && !existsSync(absolute)) continue;
      if (!staged) {
        const location = relative(root, realpathSync(absolute));
        if (location.startsWith('..') || isAbsolute(location)) {
          findings.push({ path, rule: 'file resolves outside the repository' });
          continue;
        }
      }
      const content = staged ? git(['show', `:${path}`]) : readFileSync(absolute);
      checked++;
      findings.push(...inspectFile(path, content));
    }
  }
  return { checked, findings };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const result = checkRepository({ staged: process.argv.includes('--staged'), history: process.argv.includes('--history') });
    for (const finding of result.findings) console.error(`${finding.path}${finding.line ? `:${finding.line}` : ''}${finding.blob ? ` (${finding.blob})` : ''}: ${finding.rule}`);
    if (result.findings.length) process.exitCode = 1;
    else console.log(`Repository hygiene: ${result.checked} files/blobs checked; no blocked paths or credential patterns.`);
  } catch {
    console.error('Repository hygiene check could not read the Git state.');
    process.exitCode = 1;
  }
}
