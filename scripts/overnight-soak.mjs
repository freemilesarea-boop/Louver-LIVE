#!/usr/bin/env node
/**
 * Unattended long-run stability test, start to written verdict.
 *
 * Nobody is watching this one. It starts a real RTMP ingest, runs the real
 * broadcast runtime against it in stream-copy mode until a wall-clock end
 * time, shuts everything down the way the Stop button does, analyses what the
 * ingest received, and writes FINAL_RESULT.md — without anyone typing another
 * command.
 *
 *   node scripts/overnight-soak.mjs --end-at 2026-09-20T02:35:00Z
 *   node scripts/overnight-soak.mjs --duration 13h
 *
 * Capture is a rolling window: 13 hours at 5 Mbps is ~30 GB, which will not
 * fit, so the sink keeps the first and last segments and drops the middle.
 * That is what makes "did A/V drift over 13 hours" answerable at all — the
 * last segment is measured against the first.
 */
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, writeFileSync, appendFileSync, readFileSync, readdirSync, statSync } from 'node:fs'
import { dirname, join, resolve, basename, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const arg = (n, d) => {
  const i = process.argv.indexOf(`--${n}`)
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : d
}
const dur = (s) => {
  const m = /^(\d+(?:\.\d+)?)(s|m|h)$/.exec(s)
  if (!m) throw new Error(`bad duration "${s}"`)
  return Math.round(Number(m[1]) * { s: 1, m: 60, h: 3600 }[m[2]])
}

const OUT = resolve(ROOT, arg('out', 'rc-results/overnight'))
const PORT = Number(arg('port', '1940'))
const SAMPLE = Number(arg('sample', '300'))
const SEGMENT = Number(arg('segment-secs', '300'))
const KEEP_HEAD = Number(arg('keep-head', '6'))
const KEEP_TAIL = Number(arg('keep-tail', '8'))
const LABEL = 'overnight'
// Playlist fixture durations, as built by the RC harness.
const CYCLE = [95, 62, 128, 47, 83]
const CYCLE_SECS = CYCLE.reduce((a, b) => a + b, 0)

const endAt = arg('end-at', null)
const DURATION = endAt
  ? Math.max(60, Math.round((Date.parse(endAt) - Date.now()) / 1000))
  : dur(arg('duration', '13h'))
// The harness normalizes its fixtures and connects before its own clock
// starts, so broadcasting for the whole window would overshoot the wall-clock
// end. Hold that time back, and measure the real end from the connect instant.
const PREP = Number(arg('prep-allowance', '600'))
const BROADCAST_SECS = Math.max(60, DURATION - PREP)

mkdirSync(OUT, { recursive: true })
mkdirSync(join(OUT, 'ingest'), { recursive: true })
const LOG = join(OUT, 'orchestrator.log')
function say(msg) {
  const line = `[${new Date().toISOString()}] ${msg}`
  console.log(line)
  try { appendFileSync(LOG, line + '\n') } catch { /* keep going regardless */ }
}

// --- sleep prevention ------------------------------------------------------
// §37 asks the machine not to sleep mid-broadcast. On a Linux container there
// is no power management to inhibit; whether that is true here is recorded
// rather than assumed.
function preventSleep() {
  const r = spawnSync('which', ['systemd-inhibit'], { encoding: 'utf8' })
  if (r.status === 0) {
    const p = spawn('systemd-inhibit', ['--what=sleep:idle', '--why=Louver Live overnight soak', 'sleep', String(DURATION + 600)], { stdio: 'ignore', detached: true })
    p.unref()
    say(`sleep inhibited via systemd-inhibit (pid ${p.pid})`)
    return { method: 'systemd-inhibit', pid: p.pid }
  }
  say('no systemd-inhibit on this host; container has no power management to inhibit')
  return { method: 'none', note: 'Linux container: no suspend/idle path exists to inhibit' }
}

// --- children --------------------------------------------------------------
const started = new Date()
const deadline = Date.now() + DURATION * 1000
say(`overnight soak starting: ${DURATION}s, ending ${new Date(deadline).toISOString()}`)

const sleepGuard = preventSleep()

const sink = spawn(process.execPath, [
  join(ROOT, 'scripts/rtmp-sink.mjs'),
  '--port', String(PORT),
  '--out', join(OUT, 'ingest'),
  '--segments',
  '--segment-secs', String(SEGMENT),
  '--keep-head', String(KEEP_HEAD),
  '--keep-tail', String(KEEP_TAIL),
  '--rw-timeout', '20',
], { cwd: ROOT, stdio: ['ignore', 'pipe', 'pipe'], detached: false })
const sinkOut = join(OUT, 'sink.out')
writeFileSync(sinkOut, '')
sink.stdout.on('data', (d) => appendFileSync(sinkOut, d))
sink.stderr.on('data', (d) => appendFileSync(sinkOut, d))
say(`sink pid ${sink.pid} on port ${PORT}`)

await new Promise((r) => setTimeout(r, 3000))

const harnessOut = join(OUT, `${LABEL}.out`)
writeFileSync(harnessOut, '')
const harness = spawn('cargo', [
  'test', '-p', 'louver-core', '--test', 'rc_live', '--',
  '--ignored', '--nocapture', 'rc_broadcast',
], {
  cwd: ROOT,
  stdio: ['ignore', 'pipe', 'pipe'],
  env: {
    ...process.env,
    LOUVER_TEST_RTMPS_URL: `rtmp://127.0.0.1:${PORT}/live`,
    LOUVER_RC_DURATION_SECS: String(BROADCAST_SECS),
    LOUVER_RC_SAMPLE_SECS: String(SAMPLE),
    LOUVER_RC_LABEL: LABEL,
    LOUVER_RC_OUT: relative(ROOT, OUT),
  },
})
// The harness prints this the moment the ingest accepts the stream, which is
// when its own duration clock starts. Everything before it is preparation.
let liveAt = null
const watchForLive = (d) => {
  if (liveAt === null && /connected in [\d.]+s/.test(String(d))) {
    liveAt = Date.now()
    say(`broadcast is live; ${BROADCAST_SECS}s of broadcasting ends ${new Date(liveAt + BROADCAST_SECS * 1000).toISOString()}`)
  }
}
harness.stdout.on('data', (d) => { appendFileSync(harnessOut, d); watchForLive(d) })
harness.stderr.on('data', (d) => { appendFileSync(harnessOut, d); watchForLive(d) })
say(`harness pid ${harness.pid} (broadcasting ${BROADCAST_SECS}s after ${PREP}s prep allowance)`)

// The desktop shell is a separate process from the broadcast engine, and §11
// asks about both. It is observed alongside, not instead.
let shell = null
if (existsSync(join(ROOT, 'target/release/louver-desktop')) && !process.argv.includes('--no-shell')) {
  const shellOut = join(OUT, 'shell.out')
  writeFileSync(shellOut, '')
  shell = spawn(process.execPath, [
    join(ROOT, 'scripts/observe-app.mjs'),
    '--duration', `${DURATION}s`, '--sample', `${SAMPLE}s`,
    '--out', OUT, '--display', ':97',
  ], { cwd: ROOT, stdio: ['ignore', 'pipe', 'pipe'] })
  shell.stdout.on('data', (d) => appendFileSync(shellOut, d))
  shell.stderr.on('data', (d) => appendFileSync(shellOut, d))
  say(`shell observer pid ${shell.pid}`)
}

// Everything the reporter needs to describe this run, written before the run
// can fail, so a report is possible even if nothing else survives.
writeFileSync(join(OUT, 'run-context.json'), JSON.stringify({
  started_at: started.toISOString(),
  ends_at: new Date(deadline).toISOString(),
  broadcast_seconds: BROADCAST_SECS,
  window_seconds: DURATION,
  sample_seconds: SAMPLE,
  segment_seconds: SEGMENT,
  port: PORT,
  cycle: CYCLE,
  sleep_prevention: sleepGuard,
}, null, 2))

writeFileSync(join(OUT, 'pids.json'), JSON.stringify({
  orchestrator: process.pid,
  sink: sink.pid,
  harness: harness.pid,
  shell_observer: shell?.pid ?? null,
  started_at: started.toISOString(),
  ends_at: new Date(deadline).toISOString(),
  broadcast_seconds: BROADCAST_SECS,
  window_seconds: DURATION,
  ingest_url: `rtmp://127.0.0.1:${PORT}/live`,
  sleep_prevention: sleepGuard,
}, null, 2))

// --- incidents -------------------------------------------------------------
// An error during the run is data, not a reason to stop. Anything unexpected
// is timestamped and kept; the run carries on so the supervisor's own recovery
// is what gets measured.
const incidents = []
function incident(kind, detail) {
  const rec = { at: new Date().toISOString(), elapsed_s: Math.round((Date.now() - started) / 1000), kind, detail }
  incidents.push(rec)
  say(`INCIDENT ${kind}: ${detail}`)
  writeFileSync(join(OUT, 'incidents.json'), JSON.stringify(incidents, null, 2))
}

let harnessExit = null
let harnessEndedEarly = false
/** Set before the scaffolding is torn down, so our own SIGTERM is not an incident. */
let shuttingDown = false
const harnessDone = new Promise((r) => {
  harness.on('exit', (code, signal) => {
    harnessExit = { code, signal }
    // Early relative to when broadcasting actually began, not to the wall
    // deadline — preparation time varies and is not a fault.
    const expectedEnd = (liveAt ?? started.getTime()) + BROADCAST_SECS * 1000
    if (Date.now() < expectedEnd - 120_000) {
      harnessEndedEarly = true
      incident('harness-exited-early', `code=${code} signal=${signal}, ${Math.round((expectedEnd - Date.now()) / 60000)} min short of the planned ${BROADCAST_SECS}s`)
    }
    r()
  })
})
sink.on('exit', (code) => {
  if (!shuttingDown && Date.now() < deadline - 60_000) incident('sink-exited-early', `code=${code}`)
})

// A heartbeat, so a stalled overnight run is visible in the log without
// waiting for the end.
const heartbeat = setInterval(() => {
  const left = Math.round((deadline - Date.now()) / 60000)
  const csv = join(OUT, `${LABEL}.csv`)
  let last = 'no samples yet'
  try {
    const lines = readFileSync(csv, 'utf8').trim().split('\n')
    if (lines.length > 1) last = lines.at(-1)
  } catch { /* not written yet */ }
  say(`heartbeat: ${left} min left | ${last}`)
}, 30 * 60_000)

await harnessDone
clearInterval(heartbeat)
say(`harness exited: ${JSON.stringify(harnessExit)}`)

// --- shutdown --------------------------------------------------------------
// The harness already went through the app's own Stop path. What is left is
// the test scaffolding.
shuttingDown = true
try { sink?.kill('SIGTERM') } catch { /* gone */ }
if (shell) { try { shell.kill('SIGTERM') } catch { /* gone */ } }
await new Promise((r) => setTimeout(r, 5000))

// --- report ----------------------------------------------------------------
// Analysis is a separate program on purpose. This environment reclaims its
// container when the session goes idle, which killed a run once already; if
// that happens again, the report can still be produced from what is on disk:
//
//   node scripts/soak-report.mjs --out rc-results/overnight
const reporter = spawnSync(process.execPath, [
  join(ROOT, 'scripts/soak-report.mjs'),
  '--out', OUT,
  ...(process.argv.includes('--push') ? ['--push'] : []),
], { cwd: ROOT, stdio: 'inherit', timeout: 60 * 60_000 })
say(`reporter exited ${reporter.status}`)
process.exit(reporter.status ?? 1)
