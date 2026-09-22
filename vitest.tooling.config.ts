import { defineConfig } from 'vitest/config'

/**
 * The release tooling's own suite (`scripts/*.test.mjs`).
 *
 * Its own config because it is the opposite of the UI suite: no browser, no
 * React, no setup file — these tests read executable headers, start an HTTP
 * server and run the fetch script as a child process, which jsdom's globals
 * only get in the way of.
 */
export default defineConfig({
  test: {
    environment: 'node',
    include: ['scripts/**/*.test.mjs'],
  },
})
