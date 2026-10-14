// Check local source inputs before dependency installation. Generated OUT_DIR
// inputs are tied to build.rs outputs; Cargo subsequently validates generation.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(process.argv[2] ?? fileURLToPath(new URL('../', import.meta.url)));
const errors = [];
let checked = 0;
function exists(file) { return fs.existsSync(file) && fs.statSync(file).isFile(); }
function requireFile(owner, candidates, label) {
  checked++;
  if (!candidates.some(file => {
    const relative = path.relative(root, file);
    return !relative.startsWith('..') && !path.isAbsolute(relative) && exists(file);
  })) errors.push(`${path.relative(root, owner)}: ${label}`);
}
function files(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    if (['node_modules', 'target', 'dist', '.git', '.runtime', '.local', 'gen'].includes(entry.name)) return [];
    const name = path.join(directory, entry.name);
    return entry.isDirectory() ? files(name) : [name];
  });
}
// Strings stay single tokens so embedded fixture text cannot create fake modules.
function tokens(source) {
  return [...source.matchAll(/\/\/[^\n]*|\/\*[\s\S]*?\*\/|r(#+)"[\s\S]*?"\1|r"[^"]*"|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\\n])'|[A-Za-z_$][\w$]*|[^\s]/g)]
    .filter(m => !m[0].startsWith('//') && !m[0].startsWith('/*')).map(m => m[0]);
}
function literal(token) {
  if (token?.startsWith('"')) return JSON.parse(token);
  const raw = token?.match(/^r(#+)"([\s\S]*)"\1$/) ?? token?.match(/^r()"([\s\S]*)"$/);
  return raw?.[2];
}
const buildFile = path.join(root, 'src-tauri/build.rs');
const build = fs.readFileSync(buildFile, 'utf8');
for (const m of build.matchAll(/\broot\.join\("([^"\n]+)"\)/g)) {
  requireFile(buildFile, [path.resolve(path.dirname(buildFile), m[1])], `build input ${m[1]}`);
}
const outputs = new Set([...build.matchAll(/\bout\.join\("([^"\n]+)"\)/g)].map(m => m[1]));
for (const file of files(path.join(root, 'src-tauri')).filter(f => f.endsWith('.rs'))) {
  const ts = tokens(fs.readFileSync(file, 'utf8'));
  for (let i = 0; i < ts.length; i++) {
    if (!['include_str', 'include_bytes'].includes(ts[i]) || ts[i + 1] !== '!') continue;
    const value = literal(ts[i + 3]);
    if (value !== undefined) requireFile(file, [path.resolve(path.dirname(file), value)], `${ts[i]} ${value}`);
    else {
      const expression = ts.slice(i + 3, i + 16).join('');
      const generated = expression.match(/^concat!\(env!\("OUT_DIR"\),"\/([^"\n]+)"\)/);
      if (!generated || !outputs.has(generated[1])) errors.push(`${path.relative(root, file)}: unresolved ${ts[i]} expression ${expression}`);
      else checked++;
    }
  }
  const moduleRoot = ['lib.rs', 'main.rs', 'mod.rs', 'build.rs'].includes(path.basename(file))
    ? path.dirname(file) : path.join(path.dirname(file), path.basename(file, '.rs'));
  let i = 0;
  function walk(directory, attributeBase) {
    let explicit;
    while (i < ts.length) {
      const token = ts[i++];
      if (token === '}') return;
      if (token === '#' && ts[i] === '[') {
        let depth = 0;
        const start = ++i;
        while (i < ts.length) { const t = ts[i++]; if (t === '[') depth++; if (t === ']' && depth-- === 0) break; }
        const attr = ts.slice(start, i - 1);
        if (attr[0] === 'path' && attr[1] === '=') explicit = literal(attr[2]);
      } else if (token === 'mod' && /^[A-Za-z_]\w*$/.test(ts[i] ?? '')) {
        const name = ts[i++];
        if (ts[i] === ';') {
          i++;
          const candidates = explicit === undefined ? [path.join(directory, `${name}.rs`), path.join(directory, name, 'mod.rs')]
            : [path.resolve(attributeBase, explicit)];
          requireFile(file, candidates, `mod ${name}${explicit ? ` (${explicit})` : ''}`);
        } else if (ts[i] === '{') { i++; walk(path.join(directory, name), path.join(directory, name)); }
        explicit = undefined;
      } else if (token === '{') { walk(directory, attributeBase); explicit = undefined; }
      else if (token === ';') explicit = undefined;
    }
  }
  walk(moduleRoot, path.dirname(file));
}

const frontend = files(path.join(root, 'src')).filter(f => /\.(ts|js|svelte)$/.test(f));
frontend.push(path.join(root, 'vite.config.ts'), path.join(root, 'svelte.config.js'));
const pkg = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
for (const file of frontend) {
  // Static imports/re-exports and literal dynamic imports, including multiline imports.
  const source = fs.readFileSync(file, 'utf8').replace(/\/\*[\s\S]*?\*\/|\/\/[^\n]*/g, '');
  const pattern = /\b(?:import|export)\s+(?:[^;'"()]*?\bfrom\s*)?['"]([^'"]+)['"]|\bimport\s*\(\s*['"]([^'"]+)['"]\s*\)/g;
  for (const match of source.matchAll(pattern)) {
    const specifier = (match[1] ?? match[2]).split('?')[0];
    if (specifier.startsWith('.')) {
      const base = path.resolve(path.dirname(file), specifier);
      requireFile(file, [base, ...['.ts', '.js', '.svelte', '.css', '.json', '/index.ts', '/index.js'].map(ext => base + ext)], `import ${specifier}`);
    } else if (!specifier.startsWith('node:')) {
      const name = specifier.startsWith('@') ? specifier.split('/').slice(0, 2).join('/') : specifier.split('/')[0];
      checked++;
      if (!(pkg.dependencies?.[name] || pkg.devDependencies?.[name])) errors.push(`${path.relative(root, file)}: undeclared import ${specifier}`);
    }
  }
}
const configFile = path.join(root, 'src-tauri/tauri.conf.json');
for (const icon of JSON.parse(fs.readFileSync(configFile, 'utf8')).bundle.icon) {
  requireFile(configFile, [path.resolve(path.dirname(configFile), icon)], `icon ${icon}`);
}
const htmlFile = path.join(root, 'index.html');
for (const match of fs.readFileSync(htmlFile, 'utf8').matchAll(/<script\b[^>]*\bsrc=['"]([^'"]+)['"]/g)) {
  requireFile(htmlFile, [path.resolve(root, match[1].replace(/^\//, ''))], `frontend entry ${match[1]}`);
}
if (errors.length) { console.error(errors.join('\n')); process.exitCode = 1; }
else console.log(`源码引用检查通过：${checked} 项（含 build.rs 输入、生成文件声明和图标）。`);
