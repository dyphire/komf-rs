import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// dev 时把 /api 代理到本地后端；构建产物输出到 dist，供 Rust fallback_service 挂到 /。
export default defineConfig({
  plugins: [react()],
  base: '/',
  build: { outDir: 'dist', emptyOutDir: true },
  server: {
    port: 5173,
    proxy: { '/api': { target: 'http://127.0.0.1:8085', changeOrigin: true } },
  },
});
