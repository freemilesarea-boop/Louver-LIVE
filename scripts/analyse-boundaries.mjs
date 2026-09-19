#!/usr/bin/env node
/**
 * Playlist boundary analysis for a captured broadcast (§6).
 *
 * §6 asks, at every A→B, B→C, C→A transition, whether the viewer would see a
 * freeze, a black frame, a moment of silence, a timestamp jump, buffering, or
 * a reconnection. This measures each of those from the stream the ingest
 * actually received, rather than from the sender's own opinion of itself.
 *
 *   node scripts/analyse-boundaries.mjs <captured.flv> --cycle 95,62,128,47,83
 */
import { spawnSync } from 'node:child_process'
import { existsSync, writeFileSync, mkdirSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const arg = (n, d) => {
  const i = process.argv.indexOf(`--${n}`)
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : d
}

const FILE = process.argv[2]
if (!FILE || !existsSync(FILE)) {
  console.error('usage: analyse-boundaries.mjs <captured.flv> [--cycle 95,62,128,47,83]')
  process.exit(1)
}
const DURATIONS = arg('cycle', '95,62,128,47,83').split(',').map(Number)
const NAMES = arg('names', 'rc_01,rc_02,rc_03,rc_04,rc_05').split(',')
const FPS = Number(arg('fps', '30'))
const CYCLE = DURATIONS.reduce((a, b) => a + b, 0)

function ffprobe(args) {
  const bins = [
    join(ROOT, 'apps/desktop/src-tauri/binaries/ffprobe-x86_64-unknown-linux-gnu'),
    'ffprobe',
  ]
  for (const b of bins) {
    const r = spawnSync(b, args, { encoding: 'utf8', maxBuffer: 1024 * 1024 * 1024 })
    if (r.status === 0) return r.stdout
  }
  throw new Error('ffprobe failed')
}
function ffmpegBin() {
  const p = join(ROOT, 'apps/desktop/src-tauri/binaries/ffmpeg-x86_64-unknown-linux-gnu')
  return existsSync(p) ? p : 'ffmpeg'
}

console.log(`analysing ${FILE}`)
console.log(`playlist cycle ${CYCLE}s: ${NAMES.map((n, i) => `${n}=${DURATIONS[i]}s`).join(', ')}\n`)

// --- stream timeline -------------------------------------------------------
const vRaw = ffprobe(['-v', 'error', '-select_streams', 'v:0', '-show_entries', 'packet=pts_time,flags', '-of', 'csv=p=0', FILE])
const aRaw = ffprobe(['-v', 'error', '-select_streams', 'a:0', '-show_entries', 'packet=pts_time', '-of', 'csv=p=0', FILE])

const video = []
const keyframes = []
for (const line of vRaw.split('\n')) {
  const [t, flags] = line.split(',')
  const pts = Number(t)
  if (!Number.isFinite(pts)) continue
  video.push(pts)
  if (flags && flags.includes('K')) keyframes.push(pts)
}
const audio = aRaw.split('\n').map((l) => Number(l.split(',')[0])).filter(Number.isFinite)
video.sort((a, b) => a - b)
audio.sort((a, b) => a - b)

const span = video.at(-1) - video[0]
console.log(`video : ${video.length} frames, ${video[0].toFixed(3)}s .. ${video.at(-1).toFixed(3)}s`)
console.log(`audio : ${audio.length} packets, ${audio[0].toFixed(3)}s .. ${audio.at(-1).toFixed(3)}s`)
console.log(`span  : ${span.toFixed(1)}s = ${(span / CYCLE).toFixed(2)} playlist cycles\n`)

// --- boundaries ------------------------------------------------------------
const boundaries = []
for (let t = 0, i = 0; t < span; i++) {
  const idx = i % DURATIONS.length
  t += DURATIONS[idx]
  if (t >= span - 1) break
  boundaries.push({
    at: Number(t.toFixed(3)),
    from: NAMES[idx],
    to: NAMES[(idx + 1) % NAMES.length],
  })
}

const frameGap = 1 / FPS
/** Largest inter-frame gap inside a window around t. */
function gapNear(list, t, window) {
  let worst = 0
  let where = null
  for (let i = 1; i < list.length; i++) {
    if (list[i] < t - window) continue
    if (list[i - 1] > t + window) break
    const g = list[i] - list[i - 1]
    if (g > worst) { worst = g; where = list[i - 1] }
  }
  return { gap: worst, at: where }
}

/** Mean luminance of the frame at t, to detect a black frame. */
function luminanceAt(t) {
  const r = spawnSync(ffmpegBin(), [
    '-hide_banner', '-loglevel', 'error', '-ss', String(t.toFixed(3)), '-i', FILE,
    '-frames:v', '1', '-vf', 'scale=8:8,format=gray', '-f', 'rawvideo', '-',
  ], { encoding: 'buffer', maxBuffer: 1024 * 1024 })
  if (r.status !== 0 || !r.stdout?.length) return null
  return r.stdout.reduce((a, b) => a + b, 0) / r.stdout.length
}

/** Peak audio level over a short window, to detect a dropout. */
function audioPeakDb(t, dur = 0.6) {
  const r = spawnSync(ffmpegBin(), [
    '-hide_banner', '-ss', String(Math.max(0, t).toFixed(3)), '-t', String(dur),
    '-i', FILE, '-map', '0:a:0', '-af', 'volumedetect', '-f', 'null', '-',
  ], { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 })
  const m = (r.stdout + r.stderr).match(/max_volume:\s*(-?\d+(?:\.\d+)?) dB/)
  return m ? Number(m[1]) : null
}

console.log('boundary analysis')
console.log('─'.repeat(96))
console.log('  at(s)  transition        vgap(ms)  agap(ms)  luma  peak(dB)  verdict')
console.log('─'.repeat(96))

const findings = []
for (const b of boundaries) {
  const v = gapNear(video, b.at, 1.0)
  const a = gapNear(audio, b.at, 1.0)
  const luma = luminanceAt(b.at + 0.05)
  const peak = audioPeakDb(b.at - 0.3)

  const problems = []
  // A freeze: any gap materially longer than one frame.
  if (v.gap > frameGap * 2.5) problems.push(`freeze ${(v.gap * 1000).toFixed(0)}ms`)
  // A black frame right at the seam.
  if (luma !== null && luma < 8) problems.push(`black frame (luma ${luma.toFixed(1)})`)
  // Silence across the seam.
  if (peak !== null && peak < -60) problems.push(`audio dropout (${peak}dB)`)
  // An audio gap much larger than one AAC frame (21.3ms).
  if (a.gap > 0.12) problems.push(`audio gap ${(a.gap * 1000).toFixed(0)}ms`)

  const verdict = problems.length ? problems.join(', ') : 'clean'
  if (problems.length) findings.push({ ...b, problems })
  console.log(
    `${String(b.at.toFixed(1)).padStart(7)}  ${`${b.from}→${b.to}`.padEnd(16)}` +
    `${(v.gap * 1000).toFixed(0).padStart(9)} ${(a.gap * 1000).toFixed(0).padStart(9)}` +
    `${(luma ?? -1).toFixed(0).padStart(6)} ${(peak ?? 0).toFixed(1).padStart(9)}  ${verdict}`
  )
}
console.log('─'.repeat(96))

// --- whole-stream health ---------------------------------------------------
const deltas = []
for (let i = 1; i < video.length; i++) deltas.push(video[i] - video[i - 1])
const freezes = deltas.filter((d) => d > frameGap * 2.5)
const dupes = video.length - new Set(video.map((t) => Math.round(t * 1000))).size
const backwards = deltas.filter((d) => d < 0).length

// A/V skew sampled along the timeline.
const skews = []
for (let f = 0.05; f <= 0.95; f += 0.1) {
  const t = video[Math.floor(video.length * f)]
  let lo = 0, hi = audio.length - 1
  while (lo < hi) { const mid = (lo + hi) >> 1; if (audio[mid] < t) lo = mid + 1; else hi = mid }
  skews.push(Math.abs(audio[lo] - t))
}

// Keyframe cadence: YouTube wants one every 2s.
const kfGaps = []
for (let i = 1; i < keyframes.length; i++) kfGaps.push(keyframes[i] - keyframes[i - 1])
const kfMax = kfGaps.length ? Math.max(...kfGaps) : 0

const summary = {
  file: FILE,
  duration_seconds: Number(span.toFixed(2)),
  playlist_cycle_seconds: CYCLE,
  cycles_completed: Number((span / CYCLE).toFixed(2)),
  boundaries_crossed: boundaries.length,
  video_frames: video.length,
  expected_frames: Math.round(span * FPS),
  duplicate_timestamps: dupes,
  backwards_timestamps: backwards,
  freezes_over_2_5_frames: freezes.length,
  worst_freeze_ms: freezes.length ? Number((Math.max(...freezes) * 1000).toFixed(1)) : 0,
  av_skew_max_ms: Number((Math.max(...skews) * 1000).toFixed(1)),
  av_skew_first_ms: Number((skews[0] * 1000).toFixed(1)),
  av_skew_last_ms: Number((skews.at(-1) * 1000).toFixed(1)),
  keyframe_interval_max_s: Number(kfMax.toFixed(2)),
  boundary_findings: findings,
}

console.log('\nwhole-stream health')
console.log(`  frames                ${summary.video_frames} (expected ~${summary.expected_frames})`)
console.log(`  duplicate timestamps  ${summary.duplicate_timestamps}`)
console.log(`  backwards timestamps  ${summary.backwards_timestamps}`)
console.log(`  freezes > 2.5 frames  ${summary.freezes_over_2_5_frames}${summary.worst_freeze_ms ? ` (worst ${summary.worst_freeze_ms}ms)` : ''}`)
console.log(`  A/V skew              first ${summary.av_skew_first_ms}ms, last ${summary.av_skew_last_ms}ms, max ${summary.av_skew_max_ms}ms`)
console.log(`  keyframe interval max ${summary.keyframe_interval_max_s}s`)

mkdirSync(join(ROOT, 'rc-results'), { recursive: true })
const out = join(ROOT, 'rc-results', 'boundary-analysis.json')
writeFileSync(out, JSON.stringify(summary, null, 2))
console.log(`\nwritten to ${out}`)

const fail =
  summary.duplicate_timestamps > 0 ||
  summary.backwards_timestamps > 0 ||
  findings.length > 0 ||
  summary.av_skew_max_ms > 200 ||
  summary.keyframe_interval_max_s > 4
console.log(fail ? '\nFAIL — see the findings above' : '\nPASS — every boundary clean')
process.exit(fail ? 1 : 0)
