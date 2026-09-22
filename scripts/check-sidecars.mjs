#!/usr/bin/env node
/**
 * The last gate before an installer is built: are the FFmpeg sidecars in
 * `binaries/` the ones this release may ship?
 *
 *   node scripts/check-sidecars.mjs --target x86_64-pc-windows-msvc
 *
 * Three questions, each of which has an answer that would reach a customer:
 *
 *   - Is each one there at all, under the name Tauri bundles?
 *   - Is it built for the machine this installer is aimed at? A download
 *     served the wrong asset makes a bundle that looks perfect and cannot
 *     spawn FFmpeg on the customer's computer.
 *   - Is it the licence-cleared download, or the runner's own FFmpeg that
 *     `fetch-ffmpeg.mjs` falls back to for development? The fallback is
 *     dynamically linked against libraries a user's machine has no reason to
 *     have, and is not cleared for redistribution either.
 *
 * `ffmpeg-manifest.mjs --check` asks what the binaries can do; this asks what
 * they are.
 */
import { existsSync, openSync, readSync, closeSync, readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { TOOLS, sidecarName, provenanceName, SOURCES } from './sidecar-sources.mjs'
import { archOf, expectedArch, satisfies } from './binary-arch.mjs'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const BIN_DIR = join(ROOT, 'apps/desktop/src-tauri/binaries')

function headerOf(path) {
  const fd = openSync(path, 'r')
  try {
    const buf = Buffer.alloc(4096)
    const read = readSync(fd, buf, 0, 4096, 0)
    return buf.subarray(0, read)
  } finally {
    closeSync(fd)
  }
}

function main() {
  const args = process.argv.slice(2)
  const target = args.includes('--target') ? args[args.indexOf('--target') + 1] : null
  if (!target || !SOURCES[target]) {
    console.error(`usage: check-sidecars.mjs --target <${Object.keys(SOURCES).join('|')}>`)
    process.exit(1)
  }
  const want = expectedArch(target)
  if (!want) {
    console.error(`no architecture expectation for ${target}`)
    process.exit(1)
  }

  const problems = []
  for (const tool of TOOLS) {
    const path = join(BIN_DIR, sidecarName(tool, target))
    if (!existsSync(path)) {
      problems.push(`${sidecarName(tool, target)} is missing — run the sidecar fetch first`)
      continue
    }
    const got = archOf(headerOf(path))
    const said = got ? `${got.format} ${got.arch ?? 'unknown machine'}` : 'unrecognised executable'
    console.log(`  ${tool.padEnd(8)} ${said}`)
    if (!satisfies(got, want)) {
      problems.push(`${sidecarName(tool, target)} is ${said}, not ${want.format} ${want.arch}`)
    }
  }

  const source = join(BIN_DIR, provenanceName(target))
  if (!existsSync(source)) {
    problems.push(`${provenanceName(target)} is missing — where the binaries came from is not recorded`)
  } else {
    const text = readFileSync(source, 'utf8').trim()
    console.log(`  source   ${text.split('\n').join(' · ')}`)
    if (/DEVELOPMENT ONLY/.test(text)) {
      problems.push(`${provenanceName(target)} is the development fallback and must not ship`)
    }
  }

  if (problems.length) {
    for (const p of problems) console.error(`::error::${p}`)
    console.error(`\n${problems.length} problem(s). These sidecars must not be shipped.`)
    process.exit(1)
  }
  console.log(`sidecars are the downloaded ${want.format} ${want.arch} build for ${target}`)
}

main()
