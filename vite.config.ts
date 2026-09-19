import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { resolve } from 'node:path'

// Tauri serves the built app from apps/desktop/dist.
export default defineConfig({
  plugins: [react()],
  root: 'apps/desktop',
  resolve: { alias: { '@': resolve(__dirname, 'apps/desktop/src') } },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'esnext',
    sourcemap: false,
  },
  server: { port: 5173, strictPort: true },
  clearScreen: false,
})
