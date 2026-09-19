#!/usr/bin/env node
/**
 * A local RTMP ingest endpoint, for release-candidate testing.
 *
 * It stands in for YouTube: the app publishes over a real TCP socket using the
 * real RTMP handshake, so reconnects, stream interruptions and boundary
 * behaviour are exercised against the actual protocol rather than a file.
 *
 * FFmpeg's `-listen 1` accepts exactly one connection and exits, so this wraps
 * it in a loop — which is also what makes reconnect testing possible: each
 * time the publisher drops, the sink goes back to listening.
 *
 *   node scripts/rtmp-sink.mjs --port 1935 --out /tmp/rc/ingest
 *   node scripts/rtmp-sink.mjs --port 1935 --discard        # measure only
 */
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, appendFileSync, writeFileSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

function arg(n, d) {
  const i = process.argv.indexOf(`--${n}`)
  return i >= 0 && process.argv[i + 1] && !process.argv[i + 1].startsWith('--') ? process.argv[i + 1] : d
}
const has = (n) => process.argv.includes(`--${n}`)

const PORT = Number(arg('port', '1935'))
const APP = arg('app', 'live')
const KEY = arg('key', 'rc-test')
const OUT_DIR = resolve(ROOT, arg('out', 'rc-results/ingest'))
const DISCARD = has('discard')

function findFfmpeg() {
  for (const t of ['x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'x86_64-apple-darwin', 'aarch64-apple-darwin']) {
    const p = join(ROOT, 'apps/desktop/src-tauri/binaries', `ffmpeg-${t}`)
    if (existsSync(p)) return p
  }
  const r = spawnSync('which', ['ffmpeg'], { encoding: 'utf8' })
  if (r.status === 0) return r.stdout.trim()
  throw new Error('no ffmpeg found')
}

const FFMPEG = findFfmpeg()
mkdirSync(OUT_DIR, { recursive: true })
const EVENTS = join(OUT_DIR, 'sink-events.log')
writeFileSync(EVENTS, '')

let session = 0
let stopping = false
/** Exactly one listener may hold the port at a time. */
let active = null
/** Consecutive bind failures, for backing off instead of spinning. */
let bindFailures = 0
const stats = { sessions: 0, totalBytes: 0, firstConnectAt: null, lastDisconnectAt: null, gaps: [] }

function log(msg) {
  const line = `[${new Date().toISOString()}] ${msg}`
  console.log(line)
  appendFileSync(EVENTS, line + '\n')
}

function listen() {
  if (stopping || active) return // never run two listeners on one port
  session += 1
  const n = session
  const target = DISCARD ? '-' : join(OUT_DIR, `session-${String(n).padStart(3, '0')}.flv`)
  const args = [
    '-hide_banner', '-loglevel', 'info', '-nostdin',
    '-listen', '1', '-timeout', '3600',
    // Drop a publisher that has gone silent, the way a real ingest does.
    // Without this the listener sits on a dead connection for its full accept
    // timeout and never frees the port, so a reconnecting publisher finds
    // nothing listening — which looks like a product failure but is not.
    '-rw_timeout', String(Number(arg('rw-timeout', '15')) * 1_000_000),
    '-f', 'flv', '-i', `rtmp://0.0.0.0:${PORT}/${APP}/${KEY}`,
    '-c', 'copy', '-f', DISCARD ? 'null' : 'flv', '-y', target,
  ]
  const p = spawn(FFMPEG, args, { stdio: ['ignore', 'ignore', 'pipe'] })
  active = p
  let connected = false
  let bindFailed = false

  p.stderr.on('data', (d) => {
    const s = String(d).trim()
    if (!s) return
    // "Input #0" is printed the moment the publisher's stream is parsed, which
    // is the real connect instant. Keying off any stderr line would time the
    // disconnect message instead and make reconnect gaps meaningless.
    if (!connected && s.includes('Input #0')) {
      connected = true
      const now = Date.now()
      if (stats.firstConnectAt === null) stats.firstConnectAt = now
      else if (stats.lastDisconnectAt) {
        stats.gaps.push(Number(((now - stats.lastDisconnectAt) / 1000).toFixed(1)))
      }
      stats.sessions += 1
      log(`session ${n}: publisher connected`)
    }
    // "Error during demuxing" is the normal end-of-stream from a publisher
    // that went away; it is not an ingest fault.
    if (/address already in use/i.test(s)) {
      bindFailed = true
      return // reported once, below, rather than on every retry
    }
    if (/error|failed/i.test(s) && !/Error (during demuxing|retrieving a packet)/.test(s)) {
      log(`session ${n} stderr: ${s.slice(0, 300)}`)
    }
  })

  p.on('exit', (code) => {
    active = null
    if (bindFailed) {
      // Something else holds the port. Back off rather than spinning, and do
      // not count the attempt as a publisher session.
      session -= 1
      bindFailures += 1
      if (bindFailures === 1 || bindFailures % 20 === 0) {
        log(`port ${PORT} is in use (attempt ${bindFailures}); waiting`)
      }
      if (!stopping) setTimeout(listen, Math.min(1000 * bindFailures, 10000))
      return
    }
    bindFailures = 0
    if (connected) {
      stats.lastDisconnectAt = Date.now()
      if (!DISCARD && existsSync(target)) {
        const size = statSync(target).size
        stats.totalBytes += size
        log(`session ${n}: publisher disconnected (exit ${code}), ${(size / 1e6).toFixed(1)} MB received`)
      } else {
        log(`session ${n}: publisher disconnected (exit ${code})`)
      }
    }
    if (!stopping) setTimeout(listen, 200) // go back to listening for a reconnect
  })
}

function summarise() {
  const summary = {
    port: PORT,
    url: `rtmp://127.0.0.1:${PORT}/${APP}/${KEY}`,
    publisher_sessions: stats.sessions,
    total_bytes_received: stats.totalBytes,
    reconnect_gaps_seconds: stats.gaps,
    out_dir: DISCARD ? null : OUT_DIR,
  }
  writeFileSync(join(OUT_DIR, 'sink-summary.json'), JSON.stringify(summary, null, 2))
  log(`sink stopping: ${stats.sessions} publisher session(s), ${(stats.totalBytes / 1e6).toFixed(1)} MB`)
}

function shutdown() {
  stopping = true
  if (active) { try { active.kill('SIGTERM') } catch { /* gone */ } }
  summarise()
  process.exit(0)
}
process.on('SIGINT', shutdown)
process.on('SIGTERM', shutdown)

log(`RTMP sink listening on rtmp://127.0.0.1:${PORT}/${APP}/${KEY}${DISCARD ? ' (discarding)' : ` -> ${OUT_DIR}`}`)
listen()
