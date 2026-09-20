#!/usr/bin/env node
/**
 * Start the real Louver Live desktop application, in one command.
 *
 *   npm run app
 *
 * This is not a browser preview and not a mock: it launches the Tauri window
 * with the Rust backend, SQLite, the FFmpeg and ffprobe sidecars, the playlist
 * engine, the scheduler and the stream supervisor — the same code a packaged
 * build runs. The only difference is that it runs from source, which is what
 * makes the development licence available (§46).
 *
 *   npm run app -- --no-sink     don't start the local test ingest
 *   npm run app -- --check       run the checks and stop, without launching
 *
 * Everything it checks is checked by running it. "FFmpeg OK" means a binary of
 * the right architecture was executed and answered, not that a file exists —
 * a wrong-architecture binary is the failure this is most designed to catch,
 * because its error at broadcast time is unreadable.
 */
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync } from 'node:fs'
import { arch, platform } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const BIN_DIR = join(ROOT, 'apps/desktop/src-tauri/binaries')
const NO_SINK = process.argv.includes('--no-sink')
const CHECK_ONLY = process.argv.includes('--check')
const SINK_PORT = Number(
  process.argv.includes('--sink-port') ? process.argv[process.argv.indexOf('--sink-port') + 1] : 1945,
)

const C = process.stdout.isTTY
  ? { dim: '\x1b[90m', red: '\x1b[31m', green: '\x1b[32m', yellow: '\x1b[33m', bold: '\x1b[1m', off: '\x1b[0m' }
  : { dim: '', red: '', green: '', yellow: '', bold: '', off: '' }

/** Tauri's target triple for this machine. */
function hostTriple() {
  const a = arch() === 'arm64' ? 'aarch64' : 'x86_64'
  if (platform() === 'darwin') return `${a}-apple-darwin`
  if (platform() === 'win32') return `${a}-pc-windows-msvc`
  return `${a}-unknown-linux-gnu`
}

const TRIPLE = hostTriple()
const EXE = platform() === 'win32' ? '.exe' : ''

const checks = []
function record(name, ok, detail, fix) {
  checks.push({ name, ok, detail, fix })
}

function run(cmd, args, opts = {}) {
  return spawnSync(cmd, args, { encoding: 'utf8', timeout: 30_000, ...opts })
}

// --- Node -------------------------------------------------------------------
const nodeMajor = Number(process.versions.node.split('.')[0])
record(
  'Node',
  nodeMajor >= 18,
  `v${process.versions.node}`,
  'Install Node 18 or newer: https://nodejs.org  (or `brew install node`)',
)

// --- Rust -------------------------------------------------------------------
const cargo = run('cargo', ['--version'])
record(
  'Rust',
  cargo.status === 0,
  cargo.status === 0 ? cargo.stdout.trim() : 'cargo not found',
  'Install Rust: curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh',
)

// --- Frontend dependencies --------------------------------------------------
const haveModules = existsSync(join(ROOT, 'node_modules'))
const haveVite = existsSync(join(ROOT, 'node_modules/vite'))
record(
  'Frontend',
  haveModules && haveVite,
  haveModules ? 'node_modules present' : 'node_modules missing',
  'npm install',
)

// --- Tauri CLI --------------------------------------------------------------
const haveTauri = existsSync(join(ROOT, 'node_modules/@tauri-apps/cli'))
record('Tauri', haveTauri, haveTauri ? '@tauri-apps/cli installed' : 'CLI missing', 'npm install')

// --- FFmpeg and ffprobe -----------------------------------------------------
//
// The sidecars are fetched if absent, then *executed*. A binary built for the
// other Mac architecture exists, is executable, and fails with "Bad CPU type in
// executable" — so existence proves nothing and only running it does.
mkdirSync(BIN_DIR, { recursive: true })

function sidecar(name) {
  return join(BIN_DIR, `${name}-${TRIPLE}${EXE}`)
}

function versionOf(bin) {
  if (!existsSync(bin)) return { ok: false, detail: 'not installed' }
  const r = run(bin, ['-version'])
  if (r.status !== 0) {
    const why = `${r.stderr || r.error?.message || ''}`.split('\n')[0]?.trim() || `exit ${r.status}`
    // "Bad CPU type in executable" is what macOS says about a binary built for
    // the other architecture. Other loaders phrase it differently, or the
    // shell tries to read it as a script and says something unrecognisable —
    // so the message is reported but never relied on.
    const wrongArch = /Bad CPU type|cannot execute binary|Exec format error|not found/i.test(why)
    return {
      ok: false,
      exists: true,
      detail: wrongArch
        ? `present but will not run on ${TRIPLE} (wrong architecture or corrupt)`
        : why.slice(0, 120),
    }
  }
  return { ok: true, detail: (r.stdout.split('\n')[0] || '').replace(/^ffmpeg |^ffprobe /, '').slice(0, 60) }
}

let ffmpeg = versionOf(sidecar('ffmpeg'))
let ffprobe = versionOf(sidecar('ffprobe'))

if (!ffmpeg.ok || !ffprobe.ok) {
  // A binary that exists but will not run is worse than no binary: replacing it
  // is the fix, so remove the assumption that "already there" means usable.
  // A binary that is present and does not run is not a binary to keep. Replace
  // it rather than reporting it, whatever the loader's complaint was.
  const broken = ffmpeg.exists || ffprobe.exists
  const reason = broken ? 'the installed one does not run here' : 'none installed yet'
  console.log(`${C.dim}preparing FFmpeg sidecars for ${TRIPLE} (${reason})…${C.off}`)
  const force = broken ? ['--force'] : []
  const fetched = run(process.execPath, [join(ROOT, 'scripts/fetch-ffmpeg.mjs'), '--target', TRIPLE, ...force], {
    cwd: ROOT,
    stdio: 'inherit',
    timeout: 600_000,
  })
  if (fetched.status !== 0) console.log(`${C.yellow}sidecar preparation did not complete${C.off}`)
  ffmpeg = versionOf(sidecar('ffmpeg'))
  ffprobe = versionOf(sidecar('ffprobe'))
}

const FFMPEG_FIX =
  platform() === 'darwin'
    ? `Install one and re-run, e.g. \`brew install ffmpeg\`, then \`node scripts/fetch-ffmpeg.mjs --target ${TRIPLE} --force\`.\n` +
      `    Or download a static build for ${TRIPLE} and place it at apps/desktop/src-tauri/binaries/ffmpeg-${TRIPLE}.`
    : `node scripts/fetch-ffmpeg.mjs --target ${TRIPLE} --force`

record('FFmpeg', ffmpeg.ok, ffmpeg.detail, FFMPEG_FIX)
record('ffprobe', ffprobe.ok, ffprobe.detail, FFMPEG_FIX.replace(/ffmpeg-/g, 'ffprobe-'))

// --- report -----------------------------------------------------------------
console.log()
const width = Math.max(...checks.map((c) => c.name.length)) + 2
for (const c of checks) {
  const mark = c.ok ? `${C.green}OK${C.off}` : `${C.red}FAILED${C.off}`
  console.log(`  ${c.name.padEnd(width)}${mark}   ${C.dim}${c.detail}${C.off}`)
}

const failed = checks.filter((c) => !c.ok)
if (failed.length) {
  console.log(`\n${C.red}${C.bold}Cannot start.${C.off} Fix these first:\n`)
  for (const c of failed) console.log(`  ${C.bold}${c.name}${C.off} — ${c.detail}\n    ${c.fix}\n`)
  process.exit(1)
}

if (CHECK_ONLY) {
  console.log(`\n${C.green}All checks passed.${C.off}`)
  process.exit(0)
}

// --- local test ingest ------------------------------------------------------
//
// The "로컬 방송 테스트" button publishes over RTMP when something is listening,
// and falls back to a file when nothing is. Starting a listener here is what
// makes that button exercise the real network path without the user setting
// anything up.
let sink = null
if (!NO_SINK) {
  sink = spawn(
    process.execPath,
    [
      join(ROOT, 'scripts/rtmp-sink.mjs'),
      '--port', String(SINK_PORT),
      '--key', 'louver-test',
      '--discard',
      // A long read timeout on purpose. A short one makes the listener abort
      // and re-bind every few seconds, and a broadcast that starts inside one
      // of those gaps has to reconnect before it can publish — which looks
      // like a fault in the app and is not one.
      '--rw-timeout', '3600',
    ],
    { cwd: ROOT, stdio: ['ignore', 'ignore', 'ignore'] },
  )
  sink.on('error', () => { sink = null })
  console.log(`  ${C.dim}local test ingest listening on rtmp://127.0.0.1:${SINK_PORT}/live${C.off}`)
}

console.log(`\n${C.bold}Starting Louver Live…${C.off}`)
console.log(`${C.dim}The first run compiles the Rust backend, which takes a few minutes.${C.off}\n`)

const app = spawn('npm', ['run', 'tauri', '--', 'dev'], { cwd: ROOT, stdio: 'inherit' })

function shutdown(code) {
  if (sink) { try { sink.kill('SIGTERM') } catch { /* already gone */ } }
  process.exit(code ?? 0)
}
app.on('exit', (code) => shutdown(code ?? 0))
process.on('SIGINT', () => { try { app.kill('SIGINT') } catch { /* gone */ } })
process.on('SIGTERM', () => { try { app.kill('SIGTERM') } catch { /* gone */ } })
