import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { resolve } from 'node:path'

/**
 * The cloud front end.
 *
 * `@` still points at the desktop's source, because the UI primitives, the
 * formatters and the types are shared rather than copied — the difference
 * between the two builds is the transport, not the components.
 */
export default defineConfig({
  plugins: [react()],
  root: 'apps/web',
  resolve: { alias: { '@': resolve(__dirname, 'apps/desktop/src') } },
  build: { outDir: 'dist', emptyOutDir: true, target: 'esnext', sourcemap: false },
  server: { port: 5174, strictPort: true },
})
