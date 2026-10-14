import assert from 'node:assert/strict';
import test from 'node:test';
import { accountName, accountToAdd, filterAccounts, progressForAccount, cancelledLoginState } from '../src/lib/ui.ts';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { parseTheme, readTheme, resolveTheme, writeTheme } from '../src/lib/theme.ts';

const accounts = [
  { screen_name: 'Alice', display_name: 'Alice 的档案' },
  { screen_name: 'Bob_2', display_name: '旅行 PHOTO' },
];

test('搜索同时匹配用户名、@用户名、显示名，忽略大小写', () => {
  assert.deepEqual(filterAccounts(accounts, ' @ALi '), [accounts[0]]);
  assert.deepEqual(filterAccounts(accounts, 'photo'), [accounts[1]]);
  assert.deepEqual(filterAccounts(accounts, '旅行'), [accounts[1]]);
  assert.deepEqual(filterAccounts(accounts, ''), accounts);
  assert.deepEqual(filterAccounts(accounts, 'missing'), []);
});

test('主页链接用于搜索、去重，新用户名才展示添加项', () => {
  assert.deepEqual(filterAccounts(accounts, 'https://x.com/ALICE'), [accounts[0]]);
  for (const input of ['alice', '@ALICE', 'https://twitter.com/Alice/?s=20']) {
    assert.equal(accountToAdd(accounts, input), null);
  }
  assert.equal(accountToAdd(accounts, 'https://www.x.com/New_3'), 'New_3');
  assert.equal(accountToAdd(accounts, 'partial'), 'partial');
});

test('添加判定拒绝非法用户名、非主页和非 X 链接', () => {
  for (const input of ['', '@', 'two names', 'a'.repeat(16), '中文', 'home', '@Search',
    'http://x.com/Alice', 'https://example.com/Alice', 'https://x.com/Alice/status/1',
    'https://u:p@x.com/Alice', 'https://x.com:444/Alice', 'https://x.com/']) {
    assert.equal(accountName(input), null, input);
    assert.equal(accountToAdd(accounts, input), null, input);
  }
  assert.equal(accountName(' @Ab_09 '), 'Ab_09');
});

test('首次运行和损坏偏好默认浅色，系统深色不会覆盖显式选择', () => {
  for (const value of [null, '', 'invalid']) assert.equal(parseTheme(value), 'light');
  assert.equal(resolveTheme(parseTheme(null), true), 'light');
  assert.equal(resolveTheme('light', true), 'light');
  assert.equal(resolveTheme('dark', false), 'dark');
  assert.equal(resolveTheme('system', true), 'dark');
  assert.equal(resolveTheme('system', false), 'light');
});

test('主题偏好可持久保存；localStorage 不可读写时仍可运行', () => {
  const values = new Map();
  const storage = { getItem: key => values.get(key) ?? null, setItem: (key, value) => values.set(key, value) };
  for (const theme of ['light', 'dark', 'system']) {
    writeTheme(theme, storage);
    assert.equal(readTheme(storage), theme);
  }
  const unavailable = { getItem: () => { throw new Error('denied'); }, setItem: () => { throw new Error('denied'); } };
  assert.equal(readTheme(unavailable), 'light');
  assert.doesNotThrow(() => writeTheme('dark', unavailable));
});

test('归档进度只显示对应账号；重命名按稳定 ID 匹配，准备阶段按用户名匹配', () => {
  const task = { target: { user_id: '1', screen_name: 'Alice' }, added: 7 };
  assert.equal(progressForAccount(task, { user_id: '1', screen_name: 'Renamed' }), task);
  assert.equal(progressForAccount(task, { user_id: '2', screen_name: 'Alice' }), null);
  assert.equal(progressForAccount(task, { user_id: '2', screen_name: 'Bob' }), null);
  const preparing = { ...task, target: { user_id: '', screen_name: 'Alice' } };
  assert.equal(progressForAccount(preparing, { user_id: '1', screen_name: 'ALICE' }), preparing);
  assert.equal(progressForAccount(preparing, { user_id: '2', screen_name: 'Bob' }), null);
  assert.equal(progressForAccount(task, undefined), null);
  assert.equal(progressForAccount({ target: null }, { user_id: '1', screen_name: 'Alice' }), null);
});

test('取消已完成但读取旧登录状态失败时解除 pending；传输和线程失败保留状态', () => {
  const current = { pending: true, saved: true, account: { screen_name: 'Alice' }, verified_at: 1, proxy: 'original' };
  for (const error of [
    '无法读写本机登录设置，原有登录状态保持不变',
    '无法加密或解密登录状态，请在原 Windows 用户下重试或重新登录',
    '本机登录状态不完整或格式不兼容，请重新登录',
  ]) {
    assert.deepEqual(cancelledLoginState(current, error), { ...current, pending: false, saved: false, account: null, verified_at: null });
  }
  for (const error of ['无法取消登录操作', '数据操作暂不可用', new Error('disconnected')]) {
    assert.equal(cancelledLoginState(current, error), current);
  }
  assert.equal(current.pending, true);
});

test('首帧主题脚本在应用加载前与主题解析规则一致，包括存储失败', () => {
  const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8');
  const bootstrap = html.match(/<script>([\s\S]*?)<\/script>/)[1];
  for (const value of [null, 'invalid', 'light', 'dark', 'system']) {
    for (const dark of [false, true]) {
      const document = { documentElement: { dataset: {} } };
      runInNewContext(bootstrap, { document, localStorage: { getItem: () => value }, matchMedia: () => ({ matches: dark }) });
      assert.equal(document.documentElement.dataset.theme, resolveTheme(parseTheme(value), dark));
    }
  }
  const document = { documentElement: { dataset: {} } };
  runInNewContext(bootstrap, { document, localStorage: { getItem: () => { throw new Error('denied'); } }, matchMedia: () => ({ matches: true }) });
  assert.equal(document.documentElement.dataset.theme, 'light');
});
