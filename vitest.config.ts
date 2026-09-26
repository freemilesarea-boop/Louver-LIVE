import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import { resolve } from 'node:path'

export default defineConfig({
  plugins: [react()],
  resolve: { alias: { '@': resolve(__dirname, 'apps/desktop/src') } },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['apps/desktop/src/test/setup.ts'],
    // The web front end shares these primitives and this setup file, so its
    // tests run in the same suite rather than a parallel one nobody remembers.
    include: ['apps/desktop/src/**/*.test.{ts,tsx}', 'apps/web/src/**/*.test.{ts,tsx}'],
    // The journey suite has its own config and runs as a separate step, so it
    // is excluded here rather than being counted twice.
    exclude: ['apps/desktop/src/test/e2e/**'],
  },
})
