#!/usr/bin/env node
/**
 * Records exactly what FFmpeg binary a build is shipping (§15).
 *
 * §15 requires the version, provider, licence, enabled codecs, static/dynamic
 * linkage and GPL/LGPL status to be written down for whatever is actually in
 * the bundle. Doing that by hand invites drift, so this interrogates the
 * binaries and writes a manifest that goes in the release record.
 *
 *   node scripts/ffmpeg-manifest.mjs                # all present sidecars
 *   node scripts/ffmpeg-manifest.mjs --check        # fail if unfit to ship
 */
import { spawnSync } from 'node:child_process'
import { existsSync, readdirSync, readFileSync, writeFileSync, statSync } from 'node:fs'
import { dirname, join, resolve, basename } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const BIN_DIR = join(ROOT, 'apps/desktop/src-tauri/binaries')
const CHECK = process.argv.includes('--check')

function run(bin, args) {
  const r = spawnSync(bin, args, { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 })
  return r.status === 0 ? r.stdout + r.stderr : ''
}

/** Derive the licence from the build's own configuration line — not a guess. */
function licenceOf(config) {
  const nonfree = /--enable-nonfree/.test(config)
  const gpl = /--enable-gpl/.test(config)
  const v3 = /--enable-version3/.test(config)
  if (nonfree) return { id: 'NON-FREE', redistributable: false, why: 'built with --enable-nonfree; redistribution is not permitted' }
  if (gpl) return { id: v3 ? 'GPL-3.0' : 'GPL-2.0-or-later', redistributable: true, why: 'built with --enable-gpl' }
  return { id: v3 ? 'LGPL-3.0' : 'LGPL-2.1-or-later', redistributable: true, why: 'no --enable-gpl' }
}

function linkage(path) {
  const os = process.platform
  let out = ''
  if (os === 'darwin') out = run('otool', ['-L', path])
  else if (os === 'linux') out = run('ldd', [path])
  else return { static: null, shared_libraries: null, note: 'not determined on this platform' }
  if (/not a dynamic executable|statically linked/i.test(out)) {
    return { static: true, shared_libraries: 0 }
  }
  const libs = out.split('\n').filter((l) => /=>|\.dylib|\.so/.test(l)).length
  // A handful of system libraries is normal even for a "static" build; a long
  // list means the binary depends on the build machine's environment.
  return { static: libs <= 8, shared_libraries: libs }
}

function describe(path) {
  const version = run(path, ['-hide_banner', '-version'])
  const firstLine = version.split('\n')[0] ?? ''
  const configMatch = version.match(/configuration:([^\n]*)/)
  const config = configMatch ? configMatch[1].trim() : ''
  const encoders = run(path, ['-hide_banner', '-loglevel', 'error', '-encoders'])
  const protocols = run(path, ['-hide_banner', '-loglevel', 'error', '-protocols'])

  const wanted = ['libx264', 'libopenh264', 'h264_nvenc', 'h264_qsv', 'h264_amf', 'h264_videotoolbox', 'h264_mf', 'aac']
  const present = wanted.filter((e) => new RegExp(`\\b${e}\\b`).test(encoders))

  const sourceFile = join(BIN_DIR, `SOURCE-${basename(path).replace(/^ffmpeg-/, '')}.txt`)
  const provider = existsSync(sourceFile) ? readFileSync(sourceFile, 'utf8').trim() : 'UNRECORDED'

  return {
    binary: basename(path),
    size_bytes: statSync(path).size,
    version: firstLine.replace('ffmpeg version ', '').split(' Copyright')[0],
    version_line: firstLine,
    provider,
    licence: licenceOf(config),
    linkage: linkage(path),
    h264_encoders: present,
    has_aac: /\baac\b/.test(encoders),
    supports_rtmps: /\brtmps\b/.test(protocols),
    configuration: config,
  }
}

const binaries = existsSync(BIN_DIR)
  ? readdirSync(BIN_DIR).filter((f) => f.startsWith('ffmpeg-') && !f.endsWith('.txt'))
  : []

if (!binaries.length) {
  console.error('no FFmpeg sidecars found — run `npm run sidecar` first')
  process.exit(1)
}

const manifest = { generated_at: new Date().toISOString(), host: process.platform, binaries: [] }
const problems = []

for (const b of binaries) {
  const path = join(BIN_DIR, b)
  const d = describe(path)
  manifest.binaries.push(d)

  console.log(`\n${d.binary}`)
  console.log(`  version        ${d.version}`)
  console.log(`  provider       ${d.provider.split('\n')[0]}`)
  console.log(`  licence        ${d.licence.id}  (${d.licence.why})`)
  console.log(`  linkage        ${d.linkage.static ? 'static' : 'DYNAMIC'} (${d.linkage.shared_libraries} shared libs)`)
  console.log(`  H.264 encoders ${d.h264_encoders.join(', ') || 'NONE'}`)
  console.log(`  AAC            ${d.has_aac ? 'yes' : 'NO'}`)
  console.log(`  RTMPS          ${d.supports_rtmps ? 'yes' : 'NO'}`)

  // Conditions that make a binary unfit to ship.
  if (!d.licence.redistributable) problems.push(`${d.binary}: ${d.licence.why}`)
  if (d.provider === 'UNRECORDED') problems.push(`${d.binary}: provider not recorded — §15 requires it`)
  if (/DEVELOPMENT ONLY/.test(d.provider)) problems.push(`${d.binary}: built from the development fallback, which is not licence-cleared`)
  if (d.linkage.static === false) problems.push(`${d.binary}: dynamically linked against ${d.linkage.shared_libraries} libraries — it will not run on a user's machine`)
  if (!d.h264_encoders.length) problems.push(`${d.binary}: no H.264 encoder, so videos cannot be optimized`)
  if (!d.has_aac) problems.push(`${d.binary}: no AAC encoder`)
  if (!d.supports_rtmps) problems.push(`${d.binary}: no RTMPS support, so it cannot publish to YouTube`)
}

const out = join(ROOT, 'rc-results/ffmpeg-manifest.json')
try {
  writeFileSync(out, JSON.stringify(manifest, null, 2))
  console.log(`\nmanifest written to ${out}`)
} catch {
  console.log('\n(could not write the manifest file)')
}

console.log('\n' + '─'.repeat(60))
if (!problems.length) {
  console.log('All sidecars are fit to ship.')
  process.exit(0)
}
for (const p of problems) console.error(`UNFIT  ${p}`)
console.error('─'.repeat(60))
console.error(`\n${problems.length} problem(s). ${CHECK ? 'Release build blocked.' : 'Fine for development; must be fixed before release.'}`)
process.exit(CHECK ? 1 : 0)
