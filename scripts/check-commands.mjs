import assert from 'node:assert/strict';
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import ts from 'typescript';

const rust = readFileSync('src-tauri/src/lib.rs', 'utf8');
const handler = rust.match(/tauri::generate_handler!\[([\s\S]*?)\]/)[1];
const commands = handler.split(',').map(s => s.trim().split('::').at(-1)).filter(Boolean);
const source = ts.createSourceFile('program.ts', readFileSync('src/lib/program.ts', 'utf8'), ts.ScriptTarget.Latest, true);
const wrappers = new Map();
for (const declaration of source.statements) {
  if (!ts.isFunctionDeclaration(declaration) || !declaration.name) continue;
  function visit(node) {
    if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'invoke'
      && node.arguments[0] && ts.isStringLiteral(node.arguments[0])) {
      wrappers.set(node.arguments[0].text, declaration.name.text);
    }
    ts.forEachChild(node, visit);
  }
  visit(declaration);
}
const app = readFileSync('src/App.svelte', 'utf8');
const script = app.match(/<script lang="ts">([\s\S]*?)<\/script>/)[1];
const frontend = ts.createSourceFile('App.ts', script, ts.ScriptTarget.Latest, true);
const calls = new Map();
function visit(node) {
  if (ts.isCallExpression(node) && ts.isIdentifier(node.expression)) {
    calls.set(node.expression.text, frontend.getLineAndCharacterOfPosition(node.getStart()).line + 1);
  }
  ts.forEachChild(node, visit);
}
visit(frontend);
const coverage = commands.map(command => {
  const wrapper = wrappers.get(command);
  assert.ok(wrapper, `${command} 缺少 invoke 包装`);
  const line = calls.get(wrapper);
  assert.ok(line, `${command} 的 ${wrapper} 没有前端调用`);
  return { command, wrapper, file: 'src/App.svelte', line };
});
mkdirSync('.local/checks/ui2', { recursive: true });
writeFileSync('.local/checks/ui2/command-coverage.json', JSON.stringify(coverage, null, 2) + '\n');
console.log(`${coverage.length}/${commands.length} 个注册命令均有 invoke 包装及前端调用；清单：.local/checks/ui2/command-coverage.json`);
