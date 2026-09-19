#!/usr/bin/env node
/**
 * Samples the packaged Louver Live application over a long period.
 *
 * The broadcast engine's own footprint is measured by the RC harness; this
 * measures the desktop shell around it — the Tauri window, the webview and the
 * once-a-second runtime tick — which is the other half of what §11 asks for.
 *
 *   node scripts/observe-app.mjs --duration 6h --sample 5m
 */
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, writeFileSync, appendFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const arg = (n, d) => {
  const i = process.argv.indexOf(`--${n}`)
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : d
}
const dur = (s) => {
  const m = /^(\d+(?:\.\d+)?)(s|m|h)$/.exec(s)
  if (!m) throw new Error(`bad duration "${s}"`)
  return Number(m[1]) * { s: 1, m: 60, h: 3600 }[m[2]]
}

const DURATION = dur(arg('duration', '1h'))
const SAMPLE = dur(arg('sample', '5m'))
const OUT = resolve(ROOT, arg('out', 'rc-results'))
const DISPLAY = arg('display', ':95')
const BIN = join(ROOT, 'target/release/louver-desktop')

if (!existsSync(BIN)) {
  console.error(`no release binary at ${BIN} — run: cargo build --release -p louver-desktop`)
  process.exit(1)
}
mkdirSync(OUT, { recursive: true })

const csv = join(OUT, 'app-shell.csv')
writeFileSync(csv, 'elapsed_s,rss_bytes,cpu_percent,threads,alive\n')
const log = join(OUT, 'app-shell.log')
writeFileSync(log, '')
const say = (m) => {
  const l = `[${new Date().toISOString()}] ${m}`
  console.log(l)
  appendFileSync(log, l + '\n')
}

// A virtual display and a session bus, so the window and tray behave as they
// would on a real desktop.
const xvfb = spawn('Xvfb', [DISPLAY, '-screen', '0', '1440x950x24'], { stdio: 'ignore' })
await new Promise((r) => setTimeout(r, 3000))

const dbus = spawnSync('dbus-launch', ['--sh-syntax'], { encoding: 'utf8' })
const env = { ...process.env, DISPLAY }
for (const m of dbus.stdout.matchAll(/([A-Z_]+)='([^']*)'/g)) env[m[1]] = m[2]

const app = spawn(BIN, [], { env, stdio: ['ignore', 'ignore', 'pipe'] })
app.stderr.on('data', (d) => {
  const s = String(d).trim()
  if (s && /error|panic/i.test(s)) say(`app stderr: ${s.slice(0, 300)}`)
})
say(`started louver-desktop pid ${app.pid} on ${DISPLAY}`)

function sample(pid) {
  const r = spawnSync('ps', ['-o', 'rss=,%cpu=,nlwp=', '-p', String(pid)], { encoding: 'utf8' })
  if (r.status !== 0) return null
  const [rssKb, cpu, threads] = r.stdout.trim().split(/\s+/)
  return { rss: Number(rssKb) * 1024, cpu: Number(cpu), threads: Number(threads) }
}

const started = Date.now()
let first = null
const rssSamples = []
let crashed = false

app.on('exit', (code) => {
  if ((Date.now() - started) / 1000 < DURATION - 5) {
    crashed = true
    say(`APP EXITED EARLY with code ${code} after ${((Date.now() - started) / 1000).toFixed(0)}s`)
  }
})

const timer = setInterval(() => {
  const elapsed = (Date.now() - started) / 1000
  const s = sample(app.pid)
  if (s) {
    if (first === null) first = s.rss
    rssSamples.push(s.rss)
    appendFileSync(csv, `${elapsed.toFixed(0)},${s.rss},${s.cpu},${s.threads},1\n`)
    say(`${elapsed.toFixed(0)}s  rss ${(s.rss / 1048576).toFixed(1)}MB  cpu ${s.cpu}%  threads ${s.threads}`)
  } else {
    appendFileSync(csv, `${elapsed.toFixed(0)},,,,0\n`)
    say(`${elapsed.toFixed(0)}s  process gone`)
  }
  if (elapsed >= DURATION) finish()
}, SAMPLE * 1000)

function finish() {
  clearInterval(timer)
  const elapsed = (Date.now() - started) / 1000
  // Steady-state growth only: the shell allocates its webview up front.
  const half = rssSamples.slice(Math.floor(rssSamples.length / 2))
  const steady = half.length >= 2 && half[0] > 0 ? ((half.at(-1) - half[0]) / half[0]) * 100 : 0
  const summary = {
    duration_seconds: Math.round(elapsed),
    samples: rssSamples.length,
    rss_start_bytes: first,
    rss_end_bytes: rssSamples.at(-1) ?? null,
    rss_steady_growth_percent: Number(steady.toFixed(2)),
    survived: !crashed && rssSamples.length > 0,
    csv,
  }
  writeFileSync(join(OUT, 'app-shell-summary.json'), JSON.stringify(summary, null, 2))
  say(`done: ${JSON.stringify(summary)}`)
  try { app.kill('SIGTERM') } catch { /* already gone */ }
  setTimeout(() => {
    try { xvfb.kill() } catch { /* ignore */ }
    process.exit(summary.survived && Math.abs(steady) < 10 ? 0 : 1)
  }, 1500)
}

process.on('SIGINT', finish)
process.on('SIGTERM', finish)
