#!/usr/bin/env node
/**
 * Turns a finished (or abandoned) soak run into a verdict.
 *
 * Separate from the orchestrator that runs the soak, so that a run whose
 * container was reclaimed mid-flight can still be reported on from whatever
 * reached the disk:
 *
 *   node scripts/soak-report.mjs --out rc-results/overnight [--push]
 *
 * It decides nothing it cannot measure. A criterion with no measurement behind
 * it is recorded as NOT TESTED, which makes the overall result INCOMPLETE —
 * never a pass.
 */
import { spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, writeFileSync, appendFileSync, readFileSync, readdirSync, statSync } from 'node:fs'
import { dirname, join, resolve, basename } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const arg = (n, d) => {
  const i = process.argv.indexOf(`--${n}`)
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : d
}
const OUT = resolve(ROOT, arg('out', 'rc-results/overnight'))
const LABEL = 'overnight'

function say(msg) {
  const line = `[${new Date().toISOString()}] ${msg}`
  console.log(line)
  try { appendFileSync(join(OUT, 'orchestrator.log'), line + '\n') } catch { /* keep going */ }
}

const read = (p, d = null) => { try { return JSON.parse(readFileSync(p, 'utf8')) } catch { return d } }

// Written by the orchestrator when the run started. Without it the report can
// still be made; it just cannot say what was planned.
const ctx = read(join(OUT, 'run-context.json'), {})
const PORT = ctx.port ?? 1940
const SAMPLE = ctx.sample_seconds ?? 300
const BROADCAST_SECS = ctx.broadcast_seconds ?? 0
const CYCLE = ctx.cycle ?? [95, 62, 128, 47, 83]
const CYCLE_SECS = CYCLE.reduce((a, b) => a + b, 0)
const sleepGuard = ctx.sleep_prevention ?? { method: 'unknown' }
const started = ctx.started_at ? new Date(ctx.started_at) : new Date()
const incidents = read(join(OUT, 'incidents.json'), []) ?? []
// Earlier attempts at the same run that did not finish. Recorded rather than
// quietly dropped: a run that had to be restarted is part of the result.
const priorAttempts = read(join(OUT, 'prior-attempts.json'), []) ?? []

// --- process hygiene -------------------------------------------------------
const ps = spawnSync('ps', ['-eo', 'pid=,ppid=,stat=,args='], { encoding: 'utf8' }).stdout || ''
const rows = ps.split('\n').map((l) => l.trim()).filter(Boolean)
// Anything still publishing to our ingest, or still reading our concat
// manifest, is a process Stop failed to clean up.
const strayRe = new RegExp(`rtmp://127\\.0\\.0\\.1:${PORT}|manifest\\.txt`)
const strayFfmpeg = rows.filter((l) => /ffmpeg/.test(l) && strayRe.test(l))
const zombies = rows.filter((l) => /^\d+\s+\d+\s+Z/.test(l))
const hygiene = {
  stray_broadcast_ffmpeg: strayFfmpeg.length,
  stray_details: strayFfmpeg.slice(0, 5),
  zombies: zombies.length,
  zombie_details: zombies.slice(0, 5),
}
say(`hygiene: ${JSON.stringify(hygiene)}`)

// --- analysis --------------------------------------------------------------
// The harness writes its summary as its last act, so its presence is what
// says the planned run finished rather than being cut short.
const summary = read(join(OUT, `${LABEL}-summary.json`))
const harnessCompleted = summary !== null
const sinkSummary = read(join(OUT, 'ingest', 'sink-summary.json'))
const shellSummary = read(join(OUT, 'app-shell-summary.json'))

// Boundary analysis on the first and last kept pieces. The first says the
// broadcast was clean at the start; the last says it still is, hours later.
const pieces = existsSync(join(OUT, 'ingest'))
  ? readdirSync(join(OUT, 'ingest')).filter((f) => f.endsWith('.flv')).sort()
  : []
// Two pieces from each end, so each window carries enough transitions to say
// something. The very last piece is skipped: it was cut off mid-write by the
// shutdown.
const complete = pieces.length > 1 ? pieces.slice(0, -1) : pieces
const targets = [
  ...complete.slice(0, 2).map((f, i) => [`head${i ? `-${i + 1}` : ''}`, f]),
  ...complete.slice(-2).map((f, i) => [`tail${i ? `-${i + 1}` : ''}`, f]),
]
const analysed = []
const seen = new Set()
for (const [which, file] of targets) {
  if (!file || seen.has(file)) continue
  seen.add(file)
  const path = join(OUT, 'ingest', file)
  if (statSync(path).size < 1_000_000) continue
  const outPath = join(OUT, `boundaries-${which}.json`)
  say(`analysing ${which} piece ${file}`)
  const r = spawnSync(process.execPath, [
    join(ROOT, 'scripts/analyse-boundaries.mjs'), path,
    '--cycle', CYCLE.join(','), '--out', outPath,
  ], { cwd: ROOT, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, timeout: 30 * 60_000 })
  appendFileSync(join(OUT, `boundaries-${which}.txt`), (r.stdout || '') + (r.stderr || ''))
  const parsed = read(outPath)
  if (parsed) analysed.push({ which, file, ...parsed })
  else incident('boundary-analysis-failed', `${which}: ${file}`)
}

// CSV-derived series.
let rowsCsv = []
try {
  const lines = readFileSync(join(OUT, `${LABEL}.csv`), 'utf8').trim().split('\n')
  const head = lines[0].split(',')
  rowsCsv = lines.slice(1).map((l) => Object.fromEntries(l.split(',').map((v, i) => [head[i], v])))
} catch { /* no csv */ }
const num = (r, k) => Number(r[k] || 0)
const series = (k) => rowsCsv.map((r) => num(r, k))
const mean = (v) => (v.length ? v.reduce((a, b) => a + b, 0) / v.length : 0)
const peak = (v) => (v.length ? Math.max(...v) : 0)
const growth = (v) => {
  const h = v.slice(Math.floor(v.length / 2))
  return h.length >= 2 && h[0] > 0 ? ((h.at(-1) - h[0]) / h[0]) * 100 : 0
}

const runtimeSecs = summary?.duration_seconds ?? (rowsCsv.length ? num(rowsCsv.at(-1), 'elapsed_s') : 0)
const analysis = {
  started_at: summary?.started_at ?? started.toISOString(),
  ended_at: summary?.ended_at ?? new Date().toISOString(),
  planned_seconds: BROADCAST_SECS,
  actual_seconds: runtimeSecs,
  completed_planned_run: harnessCompleted,
  samples: rowsCsv.length,
  playlist_loops: Number((runtimeSecs / CYCLE_SECS).toFixed(1)),
  media_boundaries: Math.floor(runtimeSecs / CYCLE_SECS) * CYCLE.length,
  ffmpeg_cpu_mean: Number(mean(series('ffmpeg_cpu_pct')).toFixed(2)),
  ffmpeg_cpu_peak: Number(peak(series('ffmpeg_cpu_pct')).toFixed(2)),
  app_cpu_mean: Number(mean(series('app_cpu_pct')).toFixed(2)),
  app_cpu_peak: Number(peak(series('app_cpu_pct')).toFixed(2)),
  ffmpeg_rss_start: series('ffmpeg_rss_bytes')[0] ?? 0,
  ffmpeg_rss_end: series('ffmpeg_rss_bytes').at(-1) ?? 0,
  ffmpeg_rss_peak: peak(series('ffmpeg_rss_bytes')),
  ffmpeg_rss_growth_pct: Number(growth(series('ffmpeg_rss_bytes')).toFixed(2)),
  app_rss_start: series('app_rss_bytes')[0] ?? 0,
  app_rss_end: series('app_rss_bytes').at(-1) ?? 0,
  app_rss_peak: peak(series('app_rss_bytes')),
  app_rss_growth_pct: Number(growth(series('app_rss_bytes')).toFixed(2)),
  reconnects: peak(series('reconnects')),
  restarts: peak(series('restarts')),
  ffmpeg_errors: summary?.ffmpeg_errors ?? peak(series('ffmpeg_errors')),
  longest_progress_stall_s: summary?.longest_progress_stall_seconds ?? peak(series('seconds_since_progress')),
  ticks_not_live: summary?.ticks_not_live ?? null,
  samples_not_live: rowsCsv.filter((r) => r.state !== 'LIVE').length,
  samples_stalled: rowsCsv.filter((r) => r.ingest_status === 'stalled').length,
  samples_network_down: rowsCsv.filter((r) => r.network === 'down').length,
  zombies_peak: summary?.zombies_peak ?? peak(series('zombies')),
  zombies_after_shutdown: hygiene.zombies,
  stray_ffmpeg_after_shutdown: hygiene.stray_broadcast_ffmpeg,
  bytes_sent: summary?.bytes_sent ?? 0,
  throughput_mbps: summary?.throughput_mbps ?? 0,
  rtmp_publisher_sessions: sinkSummary?.publisher_sessions ?? null,
  bytes_received: sinkSummary?.total_bytes_received ?? null,
  shell_rss_start: shellSummary?.rss_start_bytes ?? null,
  shell_rss_end: shellSummary?.rss_end_bytes ?? null,
  shell_rss_growth_pct: shellSummary?.rss_steady_growth_percent ?? null,
  boundaries: analysed,
  incidents,
  harness_completed: harnessCompleted,
}
writeFileSync(join(OUT, 'analysis.json'), JSON.stringify(analysis, null, 2))

// --- verdict ---------------------------------------------------------------
// Every criterion is decided by a measurement or it is not decided at all.
const checks = []
const check = (name, verdict, detail) => checks.push({ name, verdict, detail })
const passFail = (ok, name, detail) => check(name, ok ? 'PASS' : 'FAIL', detail)

if (rowsCsv.length === 0) {
  check('the run produced samples', 'FAIL', 'no CSV rows were written')
} else {
  passFail(analysis.completed_planned_run, 'broadcast held until the scheduled end',
    `${analysis.actual_seconds}s of ${BROADCAST_SECS}s planned`)
  passFail(analysis.samples_not_live === 0, 'LIVE at every sample',
    `${analysis.samples_not_live} of ${rowsCsv.length} samples not LIVE`)
  passFail(analysis.restarts === 0, 'no unexpected process restarts', `${analysis.restarts} restart(s)`)
  passFail(analysis.ffmpeg_errors === 0, 'no FFmpeg stream errors', `${analysis.ffmpeg_errors} error line(s)`)
  // A leak is memory that keeps climbing. A negative figure means it came
  // back down, which is the opposite of the thing being tested for.
  passFail(analysis.ffmpeg_rss_growth_pct < 2, 'no FFmpeg memory leak',
    `${analysis.ffmpeg_rss_growth_pct}% steady-state growth`)
  passFail(analysis.app_rss_growth_pct < 2, 'no runtime memory leak',
    `${analysis.app_rss_growth_pct}% steady-state growth`)
  passFail(analysis.playlist_loops >= 1, 'playlist looped',
    `${analysis.playlist_loops} cycles, ${analysis.media_boundaries} media boundaries`)
  passFail(analysis.zombies_peak === 0 && analysis.zombies_after_shutdown === 0, 'no zombie processes',
    `peak ${analysis.zombies_peak}, after shutdown ${analysis.zombies_after_shutdown}`)
  passFail(analysis.stray_ffmpeg_after_shutdown === 0, 'no orphan FFmpeg after Stop',
    `${analysis.stray_ffmpeg_after_shutdown} left`)
  // No summary from the ingest means the question was not answered, which is
  // not the same as answering it badly.
  if (analysis.rtmp_publisher_sessions === null) {
    check('one unbroken RTMP session', 'NOT TESTED', 'the ingest wrote no summary')
  } else {
    passFail(analysis.rtmp_publisher_sessions === 1, 'one unbroken RTMP session',
      `${analysis.rtmp_publisher_sessions} publisher session(s)`)
  }
  // Reconnects are not automatically a failure — recovering from one is the
  // feature — but an unrecovered one is.
  check('reconnects', analysis.reconnects === 0 ? 'PASS' : 'INFO', `${analysis.reconnects} reconnect(s)`)
}

const head = analysed.find((a) => a.which.startsWith('head'))
const tail = [...analysed].reverse().find((a) => a.which.startsWith('tail'))
if (head && tail) {
  const findings = analysed.flatMap((a) => a.boundary_findings || [])
  passFail(findings.length === 0, 'clean media boundaries in both windows',
    `${analysed.reduce((n, a) => n + a.boundaries_crossed, 0)} boundaries examined across ${analysed.length} window(s), ${findings.length} finding(s)`)
  passFail(analysed.every((a) => a.duplicate_timestamps === 0 && a.backwards_timestamps === 0),
    'timestamps monotonic',
    `${analysed.reduce((n, a) => n + a.backwards_timestamps, 0)} backwards, ` +
    `${analysed.reduce((n, a) => n + a.duplicate_timestamps, 0)} duplicate across ${analysed.length} window(s)`)
  passFail(tail.av_skew_max_ms <= Math.max(60, head.av_skew_max_ms * 3), 'A/V drift did not accumulate',
    `head max ${head.av_skew_max_ms}ms at ${head.starts_at_seconds}s, tail max ${tail.av_skew_max_ms}ms at ${tail.starts_at_seconds}s`)
  passFail(tail.keyframe_interval_max_s <= 4, 'keyframe interval within YouTube limits',
    `tail max ${tail.keyframe_interval_max_s}s`)
} else {
  check('media boundary / A-V analysis', 'NOT TESTED', 'no capture window could be analysed')
}

const failed = checks.filter((c) => c.verdict === 'FAIL')
const untested = checks.filter((c) => c.verdict === 'NOT TESTED')
const overall = failed.length ? 'FAIL' : untested.length ? 'INCOMPLETE' : 'PASS'

const hrs = (s) => `${Math.floor(s / 3600)}h ${Math.round((s % 3600) / 60)}m`
const mb = (b) => (b ? `${(b / 1048576).toFixed(1)} MB` : 'n/a')
const kst = (iso) => {
  try { return new Date(iso).toLocaleString('sv-SE', { timeZone: 'Asia/Seoul' }) + ' KST' } catch { return iso }
}

const md = `# Overnight stability test — result

**OVERALL RESULT: ${overall}**

${overall === 'PASS'
  ? 'Every criterion below was measured and met.'
  : overall === 'FAIL'
    ? `${failed.length} criterion/criteria failed. See the table and the incident log.`
    : `${untested.length} criterion/criteria could not be measured, so the run is not a pass. Nothing unmeasured is recorded as passing.`}

| | |
| --- | --- |
| Test start | ${kst(analysis.started_at)} |
| Test end | ${kst(analysis.ended_at)} |
| Actual runtime | **${hrs(analysis.actual_seconds)}** (${analysis.actual_seconds}s of ${BROADCAST_SECS}s planned) |
| Average CPU | FFmpeg **${analysis.ffmpeg_cpu_mean} %**, runtime ${analysis.app_cpu_mean} % |
| Peak CPU | FFmpeg **${analysis.ffmpeg_cpu_peak} %**, runtime ${analysis.app_cpu_peak} % |
| Memory growth | FFmpeg **${analysis.ffmpeg_rss_growth_pct} %**, runtime **${analysis.app_rss_growth_pct} %** (steady state) |
| Reconnects | **${analysis.reconnects}** |
| Errors | ${analysis.ffmpeg_errors} FFmpeg error line(s), ${analysis.restarts} restart(s), ${incidents.length} incident(s) |
| Playlist loops | **${analysis.playlist_loops}** (${analysis.media_boundaries} media boundaries) |
| Release impact | ${overall === 'PASS'
    ? 'No long-run blocker found on this platform. Remaining release blockers are unchanged and listed in RELEASE_CANDIDATE_REPORT.md.'
    : 'See the failing rows — these are release blockers until resolved.'} |

## Criteria

| Criterion | Verdict | Measured |
| --- | --- | --- |
${checks.map((c) => `| ${c.name} | **${c.verdict}** | ${c.detail} |`).join('\n')}

## Measured figures

| Metric | Value |
| --- | --- |
| Samples taken | ${analysis.samples} (every ${SAMPLE}s) |
| FFmpeg RSS | ${mb(analysis.ffmpeg_rss_start)} → ${mb(analysis.ffmpeg_rss_end)}, peak ${mb(analysis.ffmpeg_rss_peak)} |
| Runtime RSS | ${mb(analysis.app_rss_start)} → ${mb(analysis.app_rss_end)}, peak ${mb(analysis.app_rss_peak)} |
| Desktop shell RSS | ${analysis.shell_rss_start === null ? 'not observed' : `${mb(analysis.shell_rss_start)} → ${mb(analysis.shell_rss_end)} (${analysis.shell_rss_growth_pct}% steady growth)`} |
| Data sent | ${(analysis.bytes_sent / 1e9).toFixed(2)} GB at ${analysis.throughput_mbps} Mbps |
| Data received by ingest | ${analysis.bytes_received === null ? 'n/a' : `${(analysis.bytes_received / 1e9).toFixed(2)} GB`} |
| RTMP publisher sessions | ${analysis.rtmp_publisher_sessions ?? 'n/a'} |
| Longest gap without progress | ${analysis.longest_progress_stall_s}s |
| Samples with ingest stalled | ${analysis.samples_stalled} |
| Samples with network down | ${analysis.samples_network_down} |
| Zombie processes | peak ${analysis.zombies_peak}, after shutdown ${analysis.zombies_after_shutdown} |
| Orphan FFmpeg after Stop | ${analysis.stray_ffmpeg_after_shutdown} |

${analysed.length ? `## Stream received by the ingest

Capture is a rolling window: keeping all of this run would have taken
${(analysis.bytes_sent / 1e9).toFixed(1)} GB, so the ingest wrote standalone pieces and kept only the first
and last few. Comparing the last against the first is how drift across the
whole run is measured without storing the whole run.

| Window | Starts at | Boundaries | Freezes | Dup ts | Backwards ts | A/V skew max | Keyframe max |
| --- | --- | --- | --- | --- | --- | --- | --- |
${analysed.map((a) => `| ${a.which} | ${a.starts_at_seconds}s | ${a.boundaries_crossed} | ${a.freezes_over_2_5_frames}${a.worst_freeze_is_at_piece_edge ? ' (at the cut)' : ''} | ${a.duplicate_timestamps} | ${a.backwards_timestamps} | ${a.av_skew_max_ms} ms | ${a.keyframe_interval_max_s} s |`).join('\n')}

${analysed.some((a) => a.worst_freeze_is_at_piece_edge) ? `A freeze marked "(at the cut)" is where the capture window was sliced out of
the stream, not something the broadcast did.
` : ''}` : '## Stream received by the ingest\n\nNo capture window could be analysed. **NOT TESTED.**\n'}

${priorAttempts.length ? `## Earlier attempts at this run

${priorAttempts.map((a) => `**Attempt ${a.attempt}** — started ${a.started_at}, died ${a.died_at} after ${a.broadcast_seconds_completed}s of a planned ${a.planned_seconds}s.

- Cause: ${a.cause}
- Application at fault: **${a.app_fault ? 'yes' : 'no'}**
- Evidence: ${a.evidence}
- Response: ${a.response}`).join('\n\n')}

` : ''}${incidents.length ? `## Incidents

| At | Elapsed | Kind | Detail |
| --- | --- | --- | --- |
${incidents.map((i) => `| ${i.at} | ${i.elapsed_s}s | ${i.kind} | ${i.detail} |`).join('\n')}
` : '## Incidents\n\nNone recorded.\n'}

## What this run does and does not establish

- Measured on **Linux (container)** against a **local RTMP ingest** — a real
  socket and a real RTMP handshake, but not YouTube. macOS, Windows and
  YouTube ingest remain **NOT TESTED**.
- Sleep prevention: ${sleepGuard.method === 'none' ? '**not applicable** — a Linux container has no suspend path to inhibit. The macOS and Windows code paths are still NOT TESTED on those systems.' : `inhibited via \`${sleepGuard.method}\`.`}
- The test runs inside this container. If the container itself is destroyed,
  the run dies with it; that is a property of the environment, not of the app.
- Figures are what was measured over ${hrs(analysis.actual_seconds)}. Nothing here is
  extrapolated to 24 hours or to other hardware.

## Files

| | |
| --- | --- |
| Per-sample metrics | \`${join(basename(dirname(OUT)), basename(OUT))}/${LABEL}.csv\` |
| Harness summary | \`${LABEL}-summary.json\` |
| Computed analysis | \`analysis.json\` |
| Runtime event log | \`${LABEL}-runtime.log\` |
| FFmpeg log | \`${LABEL}-ffmpeg.log\` |
| Orchestrator log | \`orchestrator.log\` |
| Ingest event log | \`ingest/sink-events.log\` |
| Boundary analysis | ${analysed.map((a) => `\`boundaries-${a.which}.json\``).join(', ') || 'none'} |
`

writeFileSync(join(OUT, 'FINAL_RESULT.md'), md)
say(`OVERALL RESULT: ${overall}`)
say(`written ${join(OUT, 'FINAL_RESULT.md')}`)

// Also drop a machine-readable verdict next to it.
writeFileSync(join(OUT, 'verdict.json'), JSON.stringify({ overall, checks, analysis }, null, 2))

// --- the tracked documents -------------------------------------------------
// BENCHMARK.md and RELEASE_CANDIDATE_REPORT.md each carry a marked region for
// the long-run figures, spliced here so the record is the measurement itself
// and not someone's later recollection of it.
const longRun = `### ${hrs(analysis.actual_seconds)}, ${kst(analysis.started_at)} → ${kst(analysis.ended_at)}

Real RTMP ingest, 1080p30 stream copy, one continuous FFmpeg session, run
unattended by \`npm run soak:overnight\`. **Result: ${overall}.**

| Metric | Measured |
| --- | --- |
| Runtime | ${hrs(analysis.actual_seconds)} (${analysis.actual_seconds}s) |
| FFmpeg CPU | mean **${analysis.ffmpeg_cpu_mean} %**, peak **${analysis.ffmpeg_cpu_peak} %** |
| Runtime CPU | mean ${analysis.app_cpu_mean} %, peak ${analysis.app_cpu_peak} % |
| FFmpeg RSS | ${mb(analysis.ffmpeg_rss_start)} → ${mb(analysis.ffmpeg_rss_end)}, peak ${mb(analysis.ffmpeg_rss_peak)} |
| FFmpeg steady-state growth | **${analysis.ffmpeg_rss_growth_pct} %** |
| Runtime RSS | ${mb(analysis.app_rss_start)} → ${mb(analysis.app_rss_end)}, peak ${mb(analysis.app_rss_peak)} |
| Runtime steady-state growth | **${analysis.app_rss_growth_pct} %** |
| Reconnects / restarts | **${analysis.reconnects} / ${analysis.restarts}** |
| Samples not LIVE | ${analysis.samples_not_live} of ${analysis.samples} |
| FFmpeg errors | ${analysis.ffmpeg_errors} |
| Playlist loops | **${analysis.playlist_loops}** (${analysis.media_boundaries} media boundaries) |
| RTMP publisher sessions | ${analysis.rtmp_publisher_sessions ?? 'n/a'} |
| Longest gap without progress | ${analysis.longest_progress_stall_s}s |
| Zombie / orphan processes | ${analysis.zombies_peak} / ${analysis.stray_ffmpeg_after_shutdown} |
| Data sent | ${(analysis.bytes_sent / 1e9).toFixed(2)} GB at ${analysis.throughput_mbps} Mbps |
${head && tail ? `| A/V skew, first window (${head.starts_at_seconds}s) | max ${head.av_skew_max_ms} ms |
| A/V skew, last window (${tail.starts_at_seconds}s) | max ${tail.av_skew_max_ms} ms |` : '| Boundary / A-V analysis | **NOT TESTED** — no capture window could be analysed |'}

Capture is a rolling window of standalone FLV pieces; a full capture of this
run would have been ~${Math.round((analysis.bytes_sent || 0) / 1e9)} GB. The first and last pieces are kept, which is
what makes the two A/V rows above a before-and-after rather than a single
reading.

Full detail, per-sample CSV and the criteria table: \`rc-results/overnight/FINAL_RESULT.md\`.
${overall === 'PASS' ? '' : `\n**${overall}** — see the criteria table in that file for what failed or could not be measured.\n`}
Still **NOT TESTED**: macOS, Windows, YouTube ingest, and any run longer than
this one. Nothing above is extrapolated.`

function splice(file, body) {
  const path = join(ROOT, file)
  try {
    const text = readFileSync(path, 'utf8')
    const begin = '<!-- LONG-RUN:BEGIN -->'
    const end = '<!-- LONG-RUN:END -->'
    const i = text.indexOf(begin)
    const j = text.indexOf(end)
    if (i < 0 || j < 0) { say(`no long-run markers in ${file}; left alone`); return false }
    writeFileSync(path, `${text.slice(0, i + begin.length)}\n${body}\n${text.slice(j)}`)
    say(`updated ${file}`)
    return true
  } catch (e) {
    say(`could not update ${file}: ${e.message}`)
    return false
  }
}
splice('BENCHMARK.md', longRun)
splice('RELEASE_CANDIDATE_REPORT.md', longRun)

// --- commit ----------------------------------------------------------------
// The container is not permanent. A result that exists only inside it is a
// result that can evaporate before anyone reads it.
if (process.argv.includes('--push')) {
  const git = (...a) => {
    const r = spawnSync('git', a, { cwd: ROOT, encoding: 'utf8' })
    say(`git ${a[0]} ${a[1] ?? ''} -> ${r.status}${r.stderr ? ` ${r.stderr.trim().slice(0, 200)}` : ''}`)
    return r
  }
  // rc-results/ is ignored wholesale — it holds gigabytes of capture — so the
  // small text artefacts are added explicitly.
  const keep = ['FINAL_RESULT.md', `${LABEL}.csv`, 'analysis.json', 'verdict.json', 'orchestrator.log',
    `${LABEL}-summary.json`, `${LABEL}-runtime.log`, 'incidents.json',
    ...analysed.map((a) => `boundaries-${a.which}.json`)]
    .map((f) => join(OUT, f)).filter((f) => existsSync(f))
  git('add', '-f', ...keep)
  git('add', 'BENCHMARK.md', 'RELEASE_CANDIDATE_REPORT.md')
  const r = git('commit', '-m', `Record the overnight stability run: ${overall}

${hrs(analysis.actual_seconds)} of continuous 1080p30 stream-copy broadcast to a real RTMP
ingest, unattended. FFmpeg CPU mean ${analysis.ffmpeg_cpu_mean}%, steady-state memory growth
${analysis.ffmpeg_rss_growth_pct}%, ${analysis.reconnects} reconnect(s), ${analysis.restarts} restart(s), ${analysis.playlist_loops} playlist loops.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01HQKxudLgUjptupEoppJq8Q`)
  if (r.status === 0) {
    for (let attempt = 1; attempt <= 4; attempt++) {
      const p = git('push', '-u', 'origin', 'claude/intelligent-lamport-gansqi')
      if (p.status === 0) break
      const wait = 2000 * 2 ** (attempt - 1)
      say(`push failed; retrying in ${wait / 1000}s`)
      spawnSync('sleep', [String(wait / 1000)])
    }
  }
}

process.exit(overall === 'FAIL' ? 1 : 0)
