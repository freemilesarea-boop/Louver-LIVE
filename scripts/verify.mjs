#!/usr/bin/env node
/**
 * One command that runs everything (§70).
 *
 * Steps run in the order that fails fastest and most cheaply first, so a typo
 * is reported in seconds rather than after the media suite. Every step's
 * outcome is printed at the end, and the exit code is non-zero if any failed.
 */
import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

const STEPS = [
  { name: 'ffmpeg sidecar', cmd: 'node', args: ['scripts/fetch-ffmpeg.mjs'] },
  { name: 'frontend typecheck', cmd: 'npm', args: ['run', '--silent', 'typecheck'] },
  { name: 'frontend lint', cmd: 'npm', args: ['run', '--silent', 'lint'] },
  { name: 'frontend tests', cmd: 'npx', args: ['vitest', 'run'] },
  { name: 'UI e2e tests', cmd: 'npx', args: ['vitest', 'run', '--config', 'vitest.e2e.config.ts'] },
  { name: 'rust fmt check', cmd: 'cargo', args: ['fmt', '--all', '--', '--check'] },
  { name: 'rust clippy', cmd: 'cargo', args: ['clippy', '--workspace', '--all-targets', '--', '-D', 'warnings'] },
  { name: 'rust tests', cmd: 'cargo', args: ['test', '--workspace'] },
  { name: 'frontend build', cmd: 'npm', args: ['run', '--silent', 'build:web'] },
]

const only = process.argv.includes('--only') ? process.argv[process.argv.indexOf('--only') + 1] : null
const steps = only ? STEPS.filter((s) => s.name.includes(only)) : STEPS

if (!existsSync(join(ROOT, 'node_modules'))) {
  console.error('node_modules is missing — run `npm install` first.')
  process.exit(1)
}

const results = []
for (const step of steps) {
  process.stdout.write(`\n\x1b[1m▸ ${step.name}\x1b[0m\n`)
  const started = Date.now()
  const r = spawnSync(step.cmd, step.args, { cwd: ROOT, stdio: 'inherit', shell: process.platform === 'win32' })
  const ok = r.status === 0
  results.push({ name: step.name, ok, secs: ((Date.now() - started) / 1000).toFixed(1) })
  if (!ok) break // a later step's failure would just be noise
}

console.log('\n' + '─'.repeat(52))
for (const r of results) {
  console.log(`${r.ok ? '\x1b[32mPASS\x1b[0m' : '\x1b[31mFAIL\x1b[0m'}  ${r.name.padEnd(28)} ${r.secs}s`)
}
const skipped = steps.length - results.length
if (skipped > 0) console.log(`\x1b[90mSKIP\x1b[0m  ${skipped} step(s) after the first failure\x1b[0m`)
console.log('─'.repeat(52))

const failed = results.filter((r) => !r.ok)
if (failed.length) {
  console.error(`\n${failed.length} step(s) failed.`)
  process.exit(1)
}
console.log('\nAll checks passed.')
