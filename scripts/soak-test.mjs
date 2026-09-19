#!/usr/bin/env node
/**
 * Long-running stability test (§11, §12, §55).
 *
 * One command that stands up an ingest endpoint, runs the **real** broadcast
 * runtime against it — the same BroadcastRuntime, supervisor and scheduler the
 * app uses — samples both processes, and writes everything to disk.
 *
 *   npm run soak -- --duration 24h
 *   npm run soak -- --duration 6h --profile 720p30
 *   npm run soak -- --duration 1h --destination rtmps://a.rtmps.youtube.com/live2
 *
 * With `--destination` and `--stream-key` it soaks against real YouTube. The
 * key is passed to the child process's environment only: it is never printed,
 * never written to the CSV, and never stored in the summary.
 *
 * Everything lands in `soak-results/<label>/`:
 *   samples.csv        one row per sample: memory, CPU, state, reconnects
 *   summary.json       verdicts and totals
 *   runtime.log        what the broadcast engine reported
 *   ffmpeg.log         FFmpeg's own output, stream keys masked
 *   sink-events.log    what the ingest endpoint saw
 */
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, writeFileSync, readFileSync, copyFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

const arg = (n, d) => {
  const i = process.argv.indexOf(`--${n}`)
  return i >= 0 && process.argv[i + 1] && !process.argv[i + 1].startsWith('--') ? process.argv[i + 1] : d
}
const has = (n) => process.argv.includes(`--${n}`)

function parseDuration(s) {
  const m = /^(\d+(?:\.\d+)?)(s|m|h)$/.exec(s)
  if (!m) throw new Error(`bad duration "${s}" — use 30m, 6h, 24h`)
  return Math.round(Number(m[1]) * { s: 1, m: 60, h: 3600 }[m[2]])
}

const DURATION_STR = arg('duration', '1h')
const DURATION = parseDuration(DURATION_STR)
const SAMPLE = parseDuration(arg('sample', '5m'))
const PROFILE = arg('profile', '1080p30')
const PORT = arg('port', '1935')
const DESTINATION = arg('destination', null)
const STREAM_KEY = arg('stream-key', process.env.LOUVER_TEST_STREAM_KEY ?? null)
const LABEL = arg('label', `soak-${DURATION_STR}`)
const OUT = join(ROOT, 'soak-results', LABEL)

if (has('help')) {
  console.log(readFileSync(new URL(import.meta.url)).toString().split('*/')[0].replace(/^\/\*\*?/, ''))
  process.exit(0)
}

mkdirSync(OUT, { recursive: true })

const usingYouTube = Boolean(DESTINATION)
console.log('Louver Live soak test')
console.log(`  duration    ${DURATION_STR} (${DURATION}s)`)
console.log(`  sampling    every ${SAMPLE}s`)
console.log(`  profile     ${PROFILE}`)
console.log(`  destination ${usingYouTube ? `${DESTINATION.replace(/\/[^/]*$/, '/…')} (real ingest)` : `local RTMP on port ${PORT}`}`)
console.log(`  results     ${OUT}`)
if (usingYouTube && !STREAM_KEY) {
  console.error('\n--destination needs --stream-key (or LOUVER_TEST_STREAM_KEY in the environment)')
  process.exit(1)
}
console.log()

// --- ingest endpoint --------------------------------------------------------
let sink = null
if (!usingYouTube) {
  sink = spawn(
    'node',
    [join(ROOT, 'scripts/rtmp-sink.mjs'), '--port', PORT, '--discard', '--rw-timeout', '20', '--out', join(OUT, 'ingest')],
    { stdio: ['ignore', 'ignore', 'ignore'] },
  )
  await new Promise((r) => setTimeout(r, 3000))
  console.log('local RTMP endpoint ready\n')
}

// --- the real runtime -------------------------------------------------------
const env = {
  ...process.env,
  LOUVER_RC_DURATION_SECS: String(DURATION),
  LOUVER_RC_SAMPLE_SECS: String(SAMPLE),
  LOUVER_RC_LABEL: LABEL,
  LOUVER_RC_OUT: `soak-results/${LABEL}`,
  LOUVER_TEST_RTMPS_URL: DESTINATION ?? `rtmp://127.0.0.1:${PORT}/live`,
}
if (STREAM_KEY) env.LOUVER_TEST_STREAM_KEY = STREAM_KEY

// The fixtures the harness needs; build them if this is a first run.
if (!existsSync(join(ROOT, 'tests/fixtures/rc'))) {
  console.log('building test fixtures (first run only)…')
  const r = spawnSync('node', [join(ROOT, 'scripts/make-rc-fixtures.mjs')], { cwd: ROOT, stdio: 'inherit' })
  if (r.status !== 0) {
    console.error('could not build fixtures')
    process.exit(1)
  }
}

console.log('starting the broadcast runtime (this normalizes the fixtures first)…\n')
const started = Date.now()
const child = spawn(
  'cargo',
  ['test', '-p', 'louver-core', '--release', '--test', 'rc_live', '--', '--ignored', '--nocapture', 'rc_broadcast'],
  { cwd: ROOT, env, stdio: ['ignore', 'pipe', 'pipe'] },
)

const transcript = []
const relay = (buf) => {
  const s = String(buf)
  transcript.push(s)
  process.stdout.write(s)
}
child.stdout.on('data', relay)
child.stderr.on('data', relay)

function finish(code) {
  const elapsed = (Date.now() - started) / 1000
  writeFileSync(join(OUT, 'console.log'), transcript.join(''))

  // The harness writes its own CSV/summary; copy them to stable names.
  for (const [from, to] of [
    [`${LABEL}.csv`, 'samples.csv'],
    [`${LABEL}-summary.json`, 'summary.json'],
    [`${LABEL}-runtime.log`, 'runtime.log'],
    [`${LABEL}-ffmpeg.log`, 'ffmpeg.log'],
  ]) {
    const src = join(OUT, from)
    if (existsSync(src)) copyFileSync(src, join(OUT, to))
  }

  let summary = null
  try {
    summary = JSON.parse(readFileSync(join(OUT, 'summary.json'), 'utf8'))
  } catch { /* the harness may have failed before writing one */ }

  console.log('\n' + '─'.repeat(64))
  if (summary) {
    const mb = (b) => (b / 1048576).toFixed(1)
    console.log(`duration            ${summary.duration_seconds}s`)
    console.log(`playlist loops      ${summary.loops_completed}`)
    console.log(`app memory          ${mb(summary.app_rss_start)}MB -> ${mb(summary.app_rss_end)}MB  (steady ${Number(summary.app_rss_steady_growth_pct).toFixed(2)}%)`)
    console.log(`ffmpeg memory       ${mb(summary.ffmpeg_rss_start)}MB -> ${mb(summary.ffmpeg_rss_end)}MB  (steady ${Number(summary.ffmpeg_rss_steady_growth_pct).toFixed(2)}%)`)
    const pct = (v) => (typeof v === 'number' ? v.toFixed(2) : '—')
    console.log(`ffmpeg CPU          mean ${pct(summary.ffmpeg_cpu_mean_pct)}%  peak ${pct(summary.ffmpeg_cpu_peak_pct)}%`)
    console.log(`throughput          ${summary.throughput_mbps} Mbps`)
    console.log(`reconnects          ${summary.reconnects}`)
    console.log(`ffmpeg errors       ${summary.ffmpeg_errors}`)
  } else {
    console.log(`ran for ${elapsed.toFixed(0)}s; no summary was produced`)
  }
  console.log(`results             ${OUT}`)
  console.log('─'.repeat(64))
  console.log(code === 0 ? '\nPASS' : '\nFAIL — see console.log and ffmpeg.log')

  if (sink) { try { sink.kill('SIGTERM') } catch { /* gone */ } }
  setTimeout(() => process.exit(code === 0 ? 0 : 1), 1500)
}

child.on('exit', finish)
process.on('SIGINT', () => { try { child.kill('SIGINT') } catch { /* gone */ } })
