import { defineConfig } from 'vite';
import path from 'node:path';
import { fileURLToPath, URL } from 'node:url';

const shared = path.resolve(__dirname, 'src/shared');

export default defineConfig(({ mode }) => ({
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
      '~icons': fileURLToPath(new URL('./src/icons', import.meta.url)),
      '#shared': shared,
      '@app': '/src/app',
      rel: './src',
      react: 'preact/compat',
      mode$: path.resolve(__dirname, 'src/a.ts'),
      env: path.resolve(__dirname, mode),
      outside: path.resolve(__dirname, '../../elsewhere'),
    },
  },
}));
