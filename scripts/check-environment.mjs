import { existsSync, readdirSync } from 'node:fs';
import { delimiter, join } from 'node:path';
import { spawnSync } from 'node:child_process';

function findTool(name, fallback) {
  const candidates = (process.env.PATH ?? '').split(delimiter).filter(Boolean)
    .map((directory) => join(directory.replace(/^"|"$/g, ''), name));
  if (fallback) candidates.push(fallback);
  return candidates.find((candidate) => existsSync(candidate)) ?? null;
}

function readVersion(executable) {
  if (!executable) return null;
  const result = spawnSync(executable, ['--version'], { encoding: 'utf8', windowsHide: true });
  return result.status === 0 ? result.stdout.trim() : null;
}

const userProfile = process.env.USERPROFILE ?? '';
const rust = findTool('rustc.exe', join(userProfile, '.cargo', 'bin', 'rustc.exe'));
const cargo = findTool('cargo.exe', join(userProfile, '.cargo', 'bin', 'cargo.exe'));
const programFiles = process.env.ProgramFiles;
const programFilesX86 = process.env['ProgramFiles(x86)'];
const vswhere = programFilesX86 ? join(programFilesX86, 'Microsoft Visual Studio', 'Installer', 'vswhere.exe') : null;
let cppTools = null;
if (vswhere && existsSync(vswhere)) {
  const result = spawnSync(vswhere, [
    '-latest', '-products', '*', '-requires',
    'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath',
  ], { encoding: 'utf8', windowsHide: true });
  cppTools = result.status === 0 ? result.stdout.trim() || null : null;
}

const webviewRoots = [programFilesX86, programFiles, process.env.LOCALAPPDATA]
  .filter(Boolean).map((directory) => join(directory, 'Microsoft', 'EdgeWebView', 'Application'));
const webview2 = webviewRoots.filter(existsSync).flatMap((directory) =>
  readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && /^\d+\./.test(entry.name)
      && existsSync(join(directory, entry.name, 'msedgewebview2.exe')))
    .map((entry) => entry.name));

const rustVersion = readVersion(rust);
const cargoVersion = readVersion(cargo);
const rustDetails = rust ? spawnSync(rust, ['-vV'], { encoding: 'utf8', windowsHide: true }) : null;
const rustHost = rustDetails?.status === 0
  ? rustDetails.stdout.match(/^host: (.+)$/m)?.[1].trim() ?? null : null;
const sdkRoot = programFilesX86 ? join(programFilesX86, 'Windows Kits', '10') : null;
const windowsSdk = sdkRoot && existsSync(join(sdkRoot, 'Lib'))
  ? readdirSync(join(sdkRoot, 'Lib'), { withFileTypes: true })
    .filter((entry) => entry.isDirectory()
      && existsSync(join(sdkRoot, 'Lib', entry.name, 'um', 'x64', 'kernel32.lib'))
      && existsSync(join(sdkRoot, 'Lib', entry.name, 'ucrt', 'x64', 'ucrt.lib'))
      && existsSync(join(sdkRoot, 'Include', entry.name, 'um', 'Windows.h')))
    .map((entry) => entry.name) : [];
const missing = [];
if (Number(process.versions.node.split('.')[0]) < 24) missing.push('Node.js 24 or later');
if (!rustVersion || !cargoVersion || !rustHost?.endsWith('-pc-windows-msvc')) missing.push('Rust MSVC toolchain');
if (!cppTools) missing.push('Microsoft C++ Build Tools');
if (windowsSdk.length === 0) missing.push('Windows SDK');
if (webview2.length === 0) missing.push('WebView2 Runtime');

console.log(JSON.stringify({
  node: process.version,
  rust: rustVersion,
  rustHost,
  cargo: cargoVersion,
  cppTools,
  windowsSdk,
  webview2,
  missing,
  nativeReady: missing.length === 0,
}, null, 2));

if (process.argv.includes('--require-native') && missing.length) process.exitCode = 1;
