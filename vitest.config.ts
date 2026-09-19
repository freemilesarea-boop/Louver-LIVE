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
    include: ['apps/desktop/src/**/*.test.{ts,tsx}'],
  },
})
