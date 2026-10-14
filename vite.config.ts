import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';

export default defineConfig(({ command, mode }) => {
  // Screenshot fixtures are available only to this explicitly selected dev server.
  const screenshot = command === 'serve' && mode === 'screenshot';
  const demoFile = (name: string) => fileURLToPath(new URL(`./src/demo/${name}.ts`, import.meta.url));
  return {
    plugins: [svelte(), ...(screenshot ? [{
      name: 'screenshot-entry',
      transformIndexHtml(html: string) {
        return html.replace('/src/main.ts', '/src/demo/main.ts');
      },
    }] : [])],
    resolve: {
      alias: screenshot ? {
        '@tauri-apps/api/core': demoFile('core'),
        '@tauri-apps/api/event': demoFile('event'),
      } : undefined,
    },
    clearScreen: false,
    // public/ holds repository documentation templates, not web assets.
    publicDir: false,
    server: {
      host: '127.0.0.1',
      port: 1420,
      strictPort: true,
      watch: {
        ignored: ['**/src-tauri/**', '**/reference/**', '**/.runtime/**', '**/.local/**'],
      },
    },
    build: {
      target: 'chrome105',
      sourcemap: false,
    },
  };
});
