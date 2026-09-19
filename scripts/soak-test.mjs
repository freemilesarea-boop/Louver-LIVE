#!/usr/bin/env node
/**
 * Long-running stability harness (§55).
 *
 * CI never runs a 24-hour test. This script lets a real machine do it, and
 * records what §65 asks for: memory growth, FFmpeg restarts, CPU and errors.
 * It drives the same pipeline the app uses — normalize once, then a single
 * looping stream-copy FFmpeg — and samples the process every interval.
 *
 *   node scripts/soak-test.mjs --duration 1h
 *   node scripts/soak-test.mjs --duration 24h --out soak-results/
 *   node scripts/soak-test.mjs --duration 10m --kill-every 2m   # exercise recovery
 */
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, writeFileSync, appendFileSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`)
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : fallback
}

function parseDuration(s) {
  const m = /^(\d+(?:\.\d+)?)(s|m|h)$/.exec(s)
  if (!m) throw new Error(`bad duration "${s}" — use 30s, 10m, 1h, 24h`)
  return Number(m[1]) * { s: 1, m: 60, h: 3600 }[m[2]]
}

const DURATION = parseDuration(arg('duration', '1h'))
const KILL_EVERY = arg('kill-every') ? parseDuration(arg('kill-every')) : null
const SAMPLE = parseDuration(arg('sample', '10s'))
const OUT = resolve(ROOT, arg('out', 'soak-results'))
const PROFILE = arg('profile', '720p30')

const ffmpeg = findTool('ffmpeg')
const ffprobe = findTool('ffprobe')

function findTool(name) {
  const exe = process.platform === 'win32' ? `${name}.exe` : name
  const triples = [
    'x86_64-pc-windows-msvc', 'aarch64-apple-darwin', 'x86_64-apple-darwin',
    'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu',
  ]
  for (const t of triples) {
    const p = join(ROOT, 'apps/desktop/src-tauri/binaries', `${name}-${t}${process.platform === 'win32' ? '.exe' : ''}`)
    if (existsSync(p)) return p
  }
  const r = spawnSync(process.platform === 'win32' ? 'where' : 'which', [exe], { encoding: 'utf8' })
  if (r.status === 0) return r.stdout.split(/\r?\n/)[0].trim()
  throw new Error('no ffmpeg found; run `npm run sidecar` first')
}

const GEOM = PROFILE === '1080p30' ? { w: 1920, h: 1080, kbps: 10000 } : { w: 1280, h: 720, kbps: 4000 }

function sh(cmd, args) {
  const r = spawnSync(cmd, args, { encoding: 'utf8' })
  if (r.status !== 0) throw new Error(`${cmd} failed: ${r.stderr?.slice(0, 400)}`)
  return r.stdout
}

/** Build three normalized clips, exactly as the app's optimize step would. */
function prepare(workDir) {
  const files = []
  const specs = [
    { name: 'soak_a', secs: 20, pattern: 'testsrc2', tone: 440 },
    { name: 'soak_b', secs: 15, pattern: 'smptebars', tone: 660 },
    { name: 'soak_c', secs: 25, pattern: 'testsrc', tone: 880 },
  ]
  for (const s of specs) {
    const out = join(workDir, `${s.name}.mp4`)
    if (!existsSync(out)) {
      console.log(`  building ${s.name}.mp4 (${s.secs}s)`)
      sh(ffmpeg, [
        '-hide_banner', '-loglevel', 'error', '-y',
        '-f', 'lavfi', '-i', `${s.pattern}=size=${GEOM.w}x${GEOM.h}:rate=30:duration=${s.secs}`,
        '-f', 'lavfi', '-i', `sine=frequency=${s.tone}:sample_rate=48000:duration=${s.secs}`,
        '-c:v', 'libx264', '-preset', 'veryfast', '-profile:v', 'high', '-level', '4.2',
        '-pix_fmt', 'yuv420p', '-b:v', `${GEOM.kbps}k`, '-maxrate', `${GEOM.kbps}k`,
        '-bufsize', `${GEOM.kbps * 2}k`, '-g', '60', '-keyint_min', '60', '-sc_threshold', '0',
        '-r', '30', '-fps_mode', 'cfr', '-video_track_timescale', '30000',
        '-c:a', 'aac', '-b:a', '192k', '-ar', '48000', '-ac', '2',
        '-movflags', '+faststart', '-map_metadata', '-1', '-avoid_negative_ts', 'make_zero',
        '-t', String(s.secs), out,
      ])
    }
    files.push(out)
  }
  const manifest = join(workDir, 'manifest.txt')
  writeFileSync(manifest, 'ffconcat version 1.0\n' + files.map((f) => `file '${f}'`).join('\n') + '\n')
  return manifest
}

function sampleProcess(pid) {
  try {
    if (process.platform === 'win32') {
      const out = sh('powershell', ['-NoProfile', '-Command',
        `$p=Get-Process -Id ${pid} -ErrorAction Stop; "$($p.WorkingSet64) $($p.CPU)"`])
      const [rss, cpu] = out.trim().split(/\s+/).map(Number)
      return { rssBytes: rss, cpuSeconds: cpu }
    }
    const out = sh('ps', ['-o', 'rss=,time=,%cpu=', '-p', String(pid)])
    const [rssKb, time, cpuPct] = out.trim().split(/\s+/)
    const parts = time.split(':').map(Number)
    const cpuSeconds = parts.length === 3
      ? parts[0] * 3600 + parts[1] * 60 + parts[2]
      : parts[0] * 60 + parts[1]
    return { rssBytes: Number(rssKb) * 1024, cpuSeconds, cpuPercent: Number(cpuPct) }
  } catch {
    return null
  }
}

async function main() {
  mkdirSync(OUT, { recursive: true })
  const workDir = join(OUT, 'fixtures')
  mkdirSync(workDir, { recursive: true })

  console.log(`Louver Live soak test`)
  console.log(`  profile   ${PROFILE} (${GEOM.w}x${GEOM.h} @ ${GEOM.kbps}kbps)`)
  console.log(`  duration  ${DURATION}s`)
  console.log(`  sampling  every ${SAMPLE}s`)
  if (KILL_EVERY) console.log(`  killing ffmpeg every ${KILL_EVERY}s to exercise recovery`)
  console.log(`  output    ${OUT}\n`)

  console.log('preparing normalized fixtures...')
  const manifest = prepare(workDir)

  const csv = join(OUT, `soak-${new Date().toISOString().replace(/[:.]/g, '-')}.csv`)
  writeFileSync(csv, 'elapsed_s,rss_bytes,cpu_percent,cpu_seconds,restarts,output_bytes,errors\n')

  const sink = join(OUT, 'soak-output.flv')
  const started = Date.now()
  let restarts = 0
  let errors = 0
  let child = null
  let stopping = false

  const args = [
    '-hide_banner', '-nostdin', '-loglevel', 'warning',
    // `-re` paces the muxer at wall-clock speed. Without it FFmpeg writes as
    // fast as the disk allows, and the CPU figure would be meaningless: a live
    // broadcast is paced by YouTube, not by the disk.
    '-re',
    '-stream_loop', '-1', '-f', 'concat', '-safe', '0', '-i', manifest,
    // The live path: a pure remux, exactly as the app runs it (§41).
    '-c', 'copy', '-f', 'flv', '-y', sink,
  ]

  function launch() {
    const c = spawn(ffmpeg, args, { stdio: ['ignore', 'ignore', 'pipe'] })
    c.stderr.on('data', (d) => {
      const s = String(d)
      if (/error|non-monotonic|invalid/i.test(s)) {
        errors++
        appendFileSync(join(OUT, 'soak-errors.log'), `[${new Date().toISOString()}] ${s}`)
      }
    })
    c.on('exit', (code) => {
      if (stopping) return
      restarts++
      console.log(`  ffmpeg exited (${code}); restarting — restart #${restarts}`)
      setTimeout(() => { child = launch() }, 2000)
    })
    return c
  }

  child = launch()
  let firstRss = null
  let lastKill = Date.now()
  /** Every RSS sample, so steady-state growth can be measured at the end. */
  const rssSamples = []

  const timer = setInterval(() => {
    const elapsed = (Date.now() - started) / 1000
    const s = child?.pid ? sampleProcess(child.pid) : null
    const outBytes = existsSync(sink) ? statSync(sink).size : 0
    if (s) {
      if (firstRss === null) firstRss = s.rssBytes
      rssSamples.push({ elapsed, rss: s.rssBytes })
    }

    appendFileSync(csv,
      `${elapsed.toFixed(0)},${s?.rssBytes ?? ''},${s?.cpuPercent ?? ''},${s?.cpuSeconds ?? ''},${restarts},${outBytes},${errors}\n`)

    const mb = s ? (s.rssBytes / 1048576).toFixed(1) : '—'
    const growth = s && firstRss ? `${(((s.rssBytes - firstRss) / firstRss) * 100).toFixed(1)}%` : '—'
    process.stdout.write(
      `\r  ${elapsed.toFixed(0)}s / ${DURATION}s  rss ${mb}MB (${growth})  cpu ${s?.cpuPercent ?? '—'}%  restarts ${restarts}  errors ${errors}   `)

    if (KILL_EVERY && (Date.now() - lastKill) / 1000 >= KILL_EVERY && child?.pid) {
      lastKill = Date.now()
      console.log(`\n  [test] killing ffmpeg to verify recovery`)
      try { process.kill(child.pid, 'SIGKILL') } catch { /* already gone */ }
    }

    if (elapsed >= DURATION) finish()
  }, SAMPLE * 1000)

  function finish() {
    stopping = true
    clearInterval(timer)
    const final = child?.pid ? sampleProcess(child.pid) : null
    if (child?.pid) { try { process.kill(child.pid, 'SIGTERM') } catch { /* gone */ } }

    const elapsed = (Date.now() - started) / 1000
    const outBytes = existsSync(sink) ? statSync(sink).size : 0
    const growthPct = final && firstRss ? ((final.rssBytes - firstRss) / firstRss) * 100 : 0

    // FFmpeg allocates its muxer and I/O buffers over the first few seconds,
    // so total growth from the very first sample always looks like a leak.
    // What matters is whether memory keeps climbing once it has settled, so
    // the leak verdict uses the second half of the run only.
    const steady = rssSamples.slice(Math.floor(rssSamples.length / 2))
    const steadyGrowthPct =
      steady.length >= 2 && steady[0].rss > 0
        ? ((steady[steady.length - 1].rss - steady[0].rss) / steady[0].rss) * 100
        : 0
    // Extrapolate the steady-state slope to 24 hours, which is the number a
    // 24/7 operator actually cares about.
    const steadySpan = steady.length >= 2 ? steady[steady.length - 1].elapsed - steady[0].elapsed : 0
    const projected24hPct = steadySpan > 0 ? (steadyGrowthPct / steadySpan) * 86400 : 0

    const summary = {
      profile: PROFILE,
      duration_seconds: Math.round(elapsed),
      ffmpeg_restarts: restarts,
      errors_logged: errors,
      rss_start_bytes: firstRss,
      rss_end_bytes: final?.rssBytes ?? null,
      rss_total_growth_percent: Number(growthPct.toFixed(2)),
      rss_steady_state_growth_percent: Number(steadyGrowthPct.toFixed(2)),
      rss_projected_24h_growth_percent: Number(projected24hPct.toFixed(1)),
      cpu_percent_last: final?.cpuPercent ?? null,
      output_bytes: outBytes,
      // With `-re` this should land near the profile bitrate; a much larger
      // number means the pacing flag was lost and the CPU figure is not
      // representative of a live broadcast.
      output_mbps: Number(((outBytes * 8) / elapsed / 1e6).toFixed(2)),
      samples: rssSamples.length,
      csv,
      verdict: {
        // Steady-state memory must be flat; a run shorter than ~1 minute has
        // too few samples to judge, so it is not failed on this.
        no_memory_leak: steady.length < 3 || Math.abs(steadyGrowthPct) < 2,
        no_unexpected_restarts: KILL_EVERY ? restarts > 0 : restarts === 0,
        no_errors: errors === 0,
        // A paced run should track the profile bitrate, not race the disk.
        paced_like_a_broadcast: (outBytes * 8) / elapsed / 1e6 < GEOM.kbps / 1000 * 1.5,
      },
    }
    writeFileSync(join(OUT, 'soak-summary.json'), JSON.stringify(summary, null, 2))

    console.log('\n\n' + '─'.repeat(52))
    console.log(`duration          ${summary.duration_seconds}s`)
    console.log(`ffmpeg restarts   ${restarts}`)
    console.log(`errors            ${errors}`)
    console.log(`rss total growth  ${summary.rss_total_growth_percent}%  (includes buffer warm-up)`)
    console.log(`rss steady-state  ${summary.rss_steady_state_growth_percent}%  -> ${summary.rss_projected_24h_growth_percent}% projected over 24h`)
    console.log(`cpu (ffmpeg)      ${summary.cpu_percent_last}%`)
    console.log(`output rate       ${summary.output_mbps} Mbps`)
    console.log(`summary           ${join(OUT, 'soak-summary.json')}`)
    console.log('─'.repeat(52))

    const pass = Object.values(summary.verdict).every(Boolean)
    console.log(pass ? '\nPASS' : '\nFAIL — see soak-errors.log')
    setTimeout(() => process.exit(pass ? 0 : 1), 500)
  }

  process.on('SIGINT', finish)
}

main().catch((e) => {
  console.error(`\nsoak test failed: ${e.message}`)
  process.exit(1)
})
