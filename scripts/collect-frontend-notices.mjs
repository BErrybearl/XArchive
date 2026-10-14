// Called by generate-notices.ps1. A memory-only production build proves which runtime packages are retained.
import fs from 'node:fs';
import path from 'node:path';
import { build } from 'vite';

const root = process.cwd();
const lock = JSON.parse(fs.readFileSync('package-lock.json', 'utf8'));
const production = new Set();
function resolveDependency(from, name) {
  let current = from;
  while (true) {
    const candidate = path.posix.join(current, 'node_modules', name);
    if (lock.packages[candidate]) return candidate;
    if (!current) throw new Error(`Unresolved production dependency: ${name}`);
    const parent = path.posix.dirname(current);
    current = parent === '.' ? '' : parent;
  }
}
function visit(key) {
  if (production.has(key)) return;
  production.add(key);
  for (const name of Object.keys(lock.packages[key].dependencies ?? {})) visit(resolveDependency(key, name));
}
for (const name of Object.keys(lock.packages[''].dependencies ?? {})) visit(resolveDependency('', name));
const retained = new Set();
await build({ logLevel: 'silent', build: { write: false }, plugins: [{
  name: 'release-notices',
  generateBundle(_options, bundle) {
    for (const chunk of Object.values(bundle)) {
      if (chunk.type !== 'chunk') continue;
      for (const [id, info] of Object.entries(chunk.modules)) {
        if (info.renderedLength <= 0) continue;
        const relative = path.relative(root, id.split('?')[0]).replaceAll('\\', '/');
        const match = relative.match(/^(.*node_modules\/(?:@[^/]+\/)?[^/]+)(?:\/|$)/);
        if (match) retained.add(match[1]);
      }
    }
  },
}] });
const packages = [...retained].sort().map(key => {
  const locked = lock.packages[key];
  if (!locked) throw new Error(`Bundled package absent from lock: ${key}`);
  const dir = path.join(root, key);
  const pkg = JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8'));
  if (pkg.version !== locked.version) throw new Error(`Installed version differs from lock: ${pkg.name}`);
  const license = typeof pkg.license === 'string' ? pkg.license : pkg.license?.type;
  if (locked.license && license && locked.license !== license) throw new Error(`License differs from lock: ${pkg.name}`);
  return {
    name: pkg.name, version: pkg.version, license: license ?? locked.license,
    repository: typeof pkg.repository === 'string' ? pkg.repository : pkg.repository?.url,
    scope: production.has(key) ? '前端生产依赖' : '前端运行时补充',
    notices: fs.readdirSync(dir).filter(name => /^(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE)(\.|-|$)/i.test(name))
      .filter(name => fs.statSync(path.join(dir, name)).isFile())
      .map(name => ({ name, text: fs.readFileSync(path.join(dir, name), 'utf8') })),
  };
});
process.stdout.write(JSON.stringify({ packages }));
