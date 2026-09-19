import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import { resolve } from 'node:path'

/**
 * UI end-to-end suite (§58).
 *
 * It drives the real React app against an in-memory implementation of the Rust
 * IPC surface, so the whole user journey — add video, build a playlist, set a
 * schedule, dry run, stop, change settings, restart and see them restored — is
 * exercised without needing a packaged desktop binary.
 */
export default defineConfig({
  plugins: [react()],
  resolve: { alias: { '@': resolve(__dirname, 'apps/desktop/src') } },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['apps/desktop/src/test/setup.ts'],
    include: ['apps/desktop/src/test/e2e/**/*.test.tsx'],
    testTimeout: 30000,
  },
})
