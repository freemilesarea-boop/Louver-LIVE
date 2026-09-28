#!/usr/bin/env node
/**
 * What three concurrent streams actually cost this machine.
 *
 * Business allows three at once and the production server is a two-core
 * Hetzner CPX22, so the question "does the plan fit the box" has to be measured
 * rather than assumed. This starts a real server, prepares a real clip, and
 * publishes one, two and three streams to local RTMP sinks while sampling the
 * CPU and RSS of every FFmpeg it spawned.
 *
 *   node scripts/capacity-check.mjs
 *
 * The numbers are this machine's, not production's — read them as a shape (does
 * a stream cost 2% of a core or 60%?), not as a promise. The one thing asserted
 * rather than reported is the invariant the shape depends on: a prepared file is
 * published with `-c copy`, so a live stream costs no encoding at all.
 */
import { spawn, spawnSync, execSync } from 'node:child_process'
import { mkdtempSync, rmSync, readFileSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const PORT = 8096
const SINKS = [19351, 19352, 19353]
const BASE = `http://127.0.0.1:${PORT}`
const SAMPLE_SECONDS = 20
const log = (...a) => console.log('[capacity]', ...a)
const fail = (m) => {
  throw new Error(m)
}

const ff = ['', '-x86_64-unknown-linux-gnu', '-aarch64-unknown-linux-gnu']
  .map((s) => join(process.cwd(), 'apps/desktop/src-tauri/binaries', 'ffmpeg' + s))
  .find((p) => existsSync(p))
if (!ff) fail('FFmpeg 사이드카가 없습니다: npm run sidecar')

const data = mkdtempSync(join(tmpdir(), 'capacity-'))
const sinks = SINKS.map((port) =>
  spawn('node', ['scripts/rtmp-sink.mjs', '--port', String(port), '--discard'], { stdio: 'ignore' }),
)
const server = spawn('./target/debug/louver-server', [], {
  env: {
    ...process.env,
    LOUVER_DATA_DIR: data,
    LOUVER_MASTER_KEY: '12'.repeat(32),
    LOUVER_FFMPEG_DIR: 'apps/desktop/src-tauri/binaries',
    LOUVER_BIND: `127.0.0.1:${PORT}`,
    LOUVER_INSECURE_COOKIES: '1',
    LOUVER_DEPLOYMENT: 'local',
  },
  stdio: ['ignore', 'pipe', 'pipe'],
})
const serverLog = []
for (const s of [server.stdout, server.stderr]) s.on('data', (c) => serverLog.push(c.toString()))

let cookie = ''
async function call(method, path, body) {
  const headers = {}
  if (cookie) headers.cookie = cookie
  let payload
  if (body instanceof FormData) payload = body
  else if (body !== undefined) {
    headers['content-type'] = 'application/json'
    payload = JSON.stringify(body)
  }
  const res = await fetch(`${BASE}${path}`, { method, headers, body: payload })
  const set = res.headers.get('set-cookie')
  if (set?.includes('louver_session=')) cookie = set.split(';')[0]
  const text = await res.text()
  let json = null
  try {
    json = JSON.parse(text)
  } catch {
    /* not json */
  }
  return { status: res.status, text, json }
}

/// Every FFmpeg this server started, with its CPU share and resident size.
function ffmpegProcesses() {
  try {
    const out = execSync('ps -eo pid,pcpu,rss,comm,args --no-headers', { encoding: 'utf8' })
    return out
      .split('\n')
      .filter((l) => l.includes('binaries/ffmpeg') && l.includes('-f concat'))
      .map((l) => {
        const [pid, pcpu, rss] = l.trim().split(/\s+/)
        return { pid: Number(pid), cpu: Number(pcpu), rssMb: Number(rss) / 1024 }
      })
  } catch {
    return []
  }
}

let ok = false
try {
  for (let i = 0; i < 80; i++) {
    try {
      await fetch(`${BASE}/health`)
      break
    } catch {
      await new Promise((r) => setTimeout(r, 250))
    }
  }
  await call('POST', '/api/auth/register', {
    name: '용량 점검',
    email: `cap-${Date.now()}@example.com`,
    password: 'correct-horse-battery',
  })
  // Business, granted the way an operator does rather than through a payment.
  const uid = (await call('GET', '/api/me')).json.id
  const grant = spawnSync(
    './target/debug/louver-server',
    ['--audit-plans'],
    { env: { ...process.env, LOUVER_DATA_DIR: data }, encoding: 'utf8' },
  )
  if (grant.status !== 0) fail(`계정 확인 실패: ${grant.stderr}`)
  // There is deliberately no API that grants a plan, so this goes through the
  // same place the operator CLI does.
  const sql = spawnSync('python3', ['-c', `
import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
c.execute("UPDATE users SET plan_id='business' WHERE id=?", (sys.argv[2],))
c.execute("INSERT INTO subscriptions (user_id, plan_id, status) VALUES (?,?,'active') "
          "ON CONFLICT(user_id) DO UPDATE SET plan_id='business', status='active'", (sys.argv[2], 'business'))
c.commit()
`, join(data, 'cloud.db'), uid], { encoding: 'utf8' })
  if (sql.status !== 0) fail(`요금제 부여 실패: ${sql.stderr}`)

  // One clip, prepared once, played by all three broadcasts.
  const clip = join(data, 'clip.mp4')
  const made = spawnSync(
    ff,
    ['-y', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=1920x1080:rate=30:duration=10',
     '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=10',
     '-c:v', 'libx264', '-preset', 'veryfast', '-b:v', '6000k', '-pix_fmt', 'yuv420p',
     '-g', '60', '-keyint_min', '60', '-sc_threshold', '0', '-r', '30', '-fps_mode', 'cfr',
     '-profile:v', 'high', '-level', '4.2',
     '-c:a', 'aac', '-b:a', '192k', '-ar', '48000', '-ac', '2', '-shortest', clip],
    { encoding: 'utf8' },
  )
  if (made.status !== 0) fail(`테스트 영상을 만들지 못했습니다: ${made.stderr}`)

  const form = new FormData()
  form.append('file', new Blob([readFileSync(clip)]), 'clip1080p.mp4')
  const up = await call('POST', '/api/media/upload', form)
  if (up.status !== 200) fail(`upload ${up.status}: ${up.text}`)
  const mediaId = up.json.id
  for (let i = 0; i < 240; i++) {
    const m = (await call('GET', `/api/media/${mediaId}`)).json
    if (m.state === 'ready') break
    if (m.state === 'failed') fail(`준비 실패: ${m.last_error}`)
    await new Promise((r) => setTimeout(r, 500))
  }
  log('1080p30 클립 준비 완료 ✓')

  const ids = []
  for (const [i, port] of SINKS.entries()) {
    const dest = await call('POST', '/api/stream-destinations', {
      label: `싱크 ${i + 1}`,
      rtmps_url: `rtmp://127.0.0.1:${port}`,
      stream_key: 'live',
    })
    if (dest.status !== 200) fail(`destination ${dest.status}: ${dest.text}`)
    const b = await call('POST', '/api/broadcasts', {
      name: `부하 ${i + 1}`,
      media_ids: [mediaId],
      destination_id: dest.json.id,
      loop_forever: true,
    })
    if (b.status !== 200) fail(`broadcast ${b.status}: ${b.text}`)
    ids.push(b.json.id)
  }

  const rows = []
  for (const [i, id] of ids.entries()) {
    const started = await call('POST', `/api/broadcasts/${id}/start`)
    if (started.status !== 200) fail(`start ${started.status}: ${started.text}`)
    // Let it settle, then sample.
    await new Promise((r) => setTimeout(r, 6000))
    const samples = []
    for (let s = 0; s < SAMPLE_SECONDS; s++) {
      samples.push(ffmpegProcesses())
      await new Promise((r) => setTimeout(r, 1000))
    }
    const worst = samples.reduce(
      (acc, procs) => {
        const cpu = procs.reduce((t, p) => t + p.cpu, 0)
        const rss = procs.reduce((t, p) => t + p.rssMb, 0)
        return { cpu: Math.max(acc.cpu, cpu), rss: Math.max(acc.rss, rss), n: Math.max(acc.n, procs.length) }
      },
      { cpu: 0, rss: 0, n: 0 },
    )
    rows.push({ streams: i + 1, ...worst })
    log(`${i + 1}개 송출: FFmpeg ${worst.n}개, CPU 최대 ${worst.cpu.toFixed(1)}%, RSS 합계 ${worst.rss.toFixed(0)}MB`)
  }

  // The invariant the numbers rest on: a prepared file is copied, not encoded.
  const argv = serverLog.join('')
  if (!argv.includes('-c copy')) fail('송출이 -c copy 로 시작되지 않았습니다 (재인코딩 중)')
  if (/-c:v (libx264|h264|hevc)/.test(argv)) fail('송출 중에 영상을 인코딩하고 있습니다')
  log('송출 경로는 재인코딩 없음 (-c copy) ✓')

  console.log('\n동시 송출  FFmpeg  CPU(최대,머신 전체 %)  RSS 합계')
  for (const r of rows) {
    console.log(
      `${String(r.streams).padStart(6)}  ${String(r.n).padStart(6)}  ${r.cpu.toFixed(1).padStart(18)}%  ${r.rss.toFixed(0).padStart(7)}MB`,
    )
  }
  console.log('\n(이 수치는 이 머신의 것입니다. production CPX22 에서 다시 재보세요.)')
  ok = true
} catch (e) {
  console.error('[capacity] FAILED:', e.message)
  console.error(serverLog.join('').split('\n').slice(-25).join('\n'))
} finally {
  server.kill('SIGKILL')
  for (const s of sinks) s.kill('SIGKILL')
  rmSync(data, { recursive: true, force: true })
  console.log(ok ? '[capacity] DONE' : '[capacity] FAILED')
  process.exit(ok ? 0 : 1)
}
