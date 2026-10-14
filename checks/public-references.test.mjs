import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const scanner = fileURLToPath(new URL('../scripts/check-public-references.mjs', import.meta.url));
test('export references reject missing modules, fixtures, imports and build inputs', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'xarchive-references-'));
  const fixture = {
    'src-tauri/build.rs': 'fn main() { root.join("licenses/MIT.txt"); out.join("generated.txt"); }',
    'src-tauri/licenses/MIT.txt': 'synthetic license',
    'src-tauri/src/lib.rs': '#[cfg(test)] #[path = "../tests/e2e/mod.rs"] mod e2e;\nconst X: &str = include_str!(r"fixture.txt");\nconst Y: &str = include_str!(concat!(env!("OUT_DIR"), "/generated.txt"));\nconst TEXT: &str = r#"mod missing; include_str!("missing.txt")"#;',
    'src-tauri/tests/e2e/mod.rs': 'mod server;',
    'src-tauri/tests/e2e/server.rs': '',
    'src-tauri/src/fixture.txt': 'fixture',
    'src/main.ts': 'import "./style.css";\nimport { x } from "./lib";\nvoid import("./lib");',
    'src/lib.ts': 'export const x = 1;',
    'src/style.css': '',
    'vite.config.ts': '', 'svelte.config.js': '',
    'package.json': '{}',
    'src-tauri/tauri.conf.json': '{"bundle":{"icon":["icons/test.png"]}}',
    'src-tauri/icons/test.png': 'synthetic icon',
    'index.html': '<script type="module" src="/src/main.ts"></script>',
  };
  try {
    for (const [relative, content] of Object.entries(fixture)) {
      const file = path.join(root, relative);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, content);
    }
    const run = () => spawnSync(process.execPath, [scanner, root], { encoding: 'utf8' });
    const baseline = run();
    assert.equal(baseline.status, 0, baseline.stderr);
    for (const relative of ['src-tauri/tests/e2e/mod.rs', 'src-tauri/tests/e2e/server.rs', 'src-tauri/src/fixture.txt', 'src/lib.ts', 'src-tauri/licenses/MIT.txt', 'src-tauri/icons/test.png']) {
      fs.unlinkSync(path.join(root, relative));
      const result = run();
      assert.notEqual(result.status, 0, relative);
      fs.writeFileSync(path.join(root, relative), fixture[relative]);
    }
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
