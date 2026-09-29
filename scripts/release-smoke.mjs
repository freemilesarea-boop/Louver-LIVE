#!/usr/bin/env node
/**
 * The release candidate, end to end, in one run.
 *
 * Everything a beta student will do, against the real server binary and the
 * real FFmpeg, with the two outside services replaced by local fakes:
 *
 *   PayApp   → a local HTTP server that answers `apiLoad.html` as documented
 *   Google   → a local HTTP server answering the token endpoint and the
 *              YouTube Data API, handing out an ingestion address that points
 *              at a local RTMP sink
 *
 * So no request in this script reaches `api.payapp.kr` or `googleapis.com`, no
 * money moves, and no YouTube channel is touched — while the bytes FFmpeg sends
 * still travel over a real RTMP connection.
 *
 *   node scripts/release-smoke.mjs
 *
 * Needs a built server (`cargo build -p louver-server`) and the FFmpeg sidecar.
 * Not part of `npm run verify`: it spawns processes and binds four ports.
 */
import { createServer } from 'node:http'
import { spawn, spawnSync } from 'node:child_process'
import { mkdtempSync, rmSync, readFileSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const APP_PORT = 8091
const PAY_PORT = 8092
const GOOGLE_PORT = 8093
const RTMP_PORT = 19350
const BASE = `http://127.0.0.1:${APP_PORT}`
const GOOGLE = `http://127.0.0.1:${GOOGLE_PORT}`

const PAY_USERID = 'release-merchant'
const LINKKEY = 'release-link-key'
const LINKVAL = 'release-link-val'
const EMAIL = `student-${Date.now()}@example.com`
const ADMIN_EMAIL = `operator-${Date.now()}@example.com`
const PASSWORD = 'correct-horse-battery'

const log = (...a) => console.log('[release]', ...a)
const step = (n, what) => console.log(`\n[release] ${String(n).padStart(2, ' ')}. ${what}`)
const fail = (m) => {
  throw new Error(m)
}

const FFMPEG = join(process.cwd(), 'apps/desktop/src-tauri/binaries', 'ffmpeg')
const ffmpegPath = ['', '-x86_64-unknown-linux-gnu', '-aarch64-unknown-linux-gnu']
  .map((s) => FFMPEG + s)
  .find((p) => existsSync(p))
if (!ffmpegPath) fail('FFmpeg 사이드카가 없습니다: npm run sidecar')

// --- fake PayApp -----------------------------------------------------------
const fakePay = createServer((req, res) => {
  let body = ''
  req.on('data', (c) => (body += c))
  req.on('end', () => {
    const f = Object.fromEntries(new URLSearchParams(body))
    res.writeHead(200, { 'Content-Type': 'text/plain' })
    if (f.cmd === 'rebillRegist') {
      res.end(
        `state=1&errno=00000&rebill_no=REL1&payurl=${encodeURIComponent(`${BASE}/billing/complete`)}`,
      )
    } else res.end('state=1&errno=00000')
  })
})

// --- fake Google -----------------------------------------------------------
const google = {
  lifecycle: 'ready',
  streamStatus: 'inactive',
  broadcastId: 'bcast-1',
  issued: 1,
  transitions: [],
  inserts: 0,
}
const broadcastJson = () => ({
  id: google.broadcastId,
  snippet: { title: '방송', description: '' },
  status: { lifeCycleStatus: google.lifecycle, privacyStatus: 'unlisted' },
  contentDetails: { enableAutoStart: true, enableAutoStop: true, boundStreamId: 'stream-1' },
})

const fakeGoogle = createServer((req, res) => {
  let body = ''
  req.on('data', (c) => (body += c))
  req.on('end', () => {
    const url = req.url
    const send = (v) => {
      res.writeHead(200, { 'Content-Type': 'application/json' })
      res.end(JSON.stringify(v))
    }
    if (url.startsWith('/token')) {
      return send({ access_token: 'access-1', refresh_token: 'refresh-1', expires_in: 3600 })
    }
    if (url.includes('/channels?')) {
      return send({
        items: [{ id: 'UC-release', snippet: { title: '수강생 채널', thumbnails: {} } }],
      })
    }
    if (url.includes('/liveStreams?') && req.method === 'POST') {
      // No `rtmpsIngestionAddress`, so the server falls back to the plain
      // address — which is the local sink, not YouTube.
      return send({
        id: 'stream-1',
        snippet: { title: '247streams' },
        cdn: {
          ingestionInfo: {
            ingestionAddress: `rtmp://127.0.0.1:${RTMP_PORT}`,
            streamName: 'live',
          },
        },
        status: { streamStatus: 'inactive' },
      })
    }
    if (url.includes('/liveStreams?')) {
      return send({
        items: [
          {
            id: 'stream-1',
            snippet: { title: '247streams' },
            cdn: { ingestionInfo: { streamName: 'live' } },
            status: { streamStatus: google.streamStatus },
          },
        ],
      })
    }
    if (url.includes('/liveBroadcasts/bind')) return send(broadcastJson())
    if (url.includes('/liveBroadcasts/transition')) {
      const to = url.split('broadcastStatus=')[1].split('&')[0]
      google.transitions.push(to)
      google.lifecycle = to
      return send(broadcastJson())
    }
    if (url.includes('/liveBroadcasts?')) {
      if (req.method === 'POST') {
        google.issued += 1
        google.broadcastId = `bcast-${google.issued}`
        google.lifecycle = 'ready'
        google.inserts += 1
        return send(broadcastJson())
      }
      if (req.method === 'PUT') return send(broadcastJson())
      return send({ items: [broadcastJson()] })
    }
    res.writeHead(404, { 'Content-Type': 'application/json' })
    res.end(JSON.stringify({ error: { message: `fake google: ${req.method} ${url}` } }))
  })
})

await new Promise((r) => fakePay.listen(PAY_PORT, '127.0.0.1', r))
await new Promise((r) => fakeGoogle.listen(GOOGLE_PORT, '127.0.0.1', r))
log(`fake PayApp on :${PAY_PORT}, fake Google on :${GOOGLE_PORT}`)

// --- a local RTMP sink, so FFmpeg publishes over a real connection ---------
const sink = spawn('node', ['scripts/rtmp-sink.mjs', '--port', String(RTMP_PORT), '--discard'], {
  stdio: ['ignore', 'pipe', 'pipe'],
})
const sinkLog = []
for (const s of [sink.stdout, sink.stderr]) s.on('data', (c) => sinkLog.push(c.toString()))

// --- the real server -------------------------------------------------------
const data = mkdtempSync(join(tmpdir(), 'release-smoke-'))
const env = {
  ...process.env,
  LOUVER_DATA_DIR: data,
  LOUVER_MASTER_KEY: 'ab'.repeat(32),
  LOUVER_WEB_DIR: 'apps/web/dist',
  LOUVER_FFMPEG_DIR: 'apps/desktop/src-tauri/binaries',
  LOUVER_BIND: `127.0.0.1:${APP_PORT}`,
  LOUVER_INSECURE_COOKIES: '1',
  LOUVER_DEPLOYMENT: 'local',
  LOUVER_PUBLIC_URL: BASE,
  PAYAPP_USERID: PAY_USERID,
  PAYAPP_LINKKEY: LINKKEY,
  PAYAPP_LINKVAL: LINKVAL,
  PAYAPP_API_URL: `http://127.0.0.1:${PAY_PORT}/oapi/apiLoad.html`,
  YOUTUBE_CLIENT_ID: 'release-client.apps.googleusercontent.com',
  YOUTUBE_CLIENT_SECRET: 'GOCSPX-release-smoke-only',
  YOUTUBE_API_BASE: `${GOOGLE}/youtube/v3`,
  YOUTUBE_TOKEN_ENDPOINT: `${GOOGLE}/token`,
  YOUTUBE_OAUTH_REDIRECT_URI: `${BASE}/api/youtube/oauth/callback`,
}

let server = null
const serverLog = []
function startServer() {
  const p = spawn('./target/debug/louver-server', [], { env, stdio: ['ignore', 'pipe', 'pipe'] })
  for (const s of [p.stdout, p.stderr]) s.on('data', (c) => serverLog.push(c.toString()))
  return p
}
async function waitForServer() {
  for (let i = 0; i < 120; i++) {
    try {
      const r = await fetch(`${BASE}/health`)
      if (r.status === 200 || r.status === 503) return
    } catch {
      /* not up yet */
    }
    await new Promise((r) => setTimeout(r, 250))
  }
  fail('서버가 뜨지 않았습니다')
}

// --- a session that keeps its cookie ---------------------------------------
let cookie = ''
async function call(method, path, body, extra = {}) {
  const headers = { ...extra.headers }
  if (cookie) headers.cookie = cookie
  let payload
  if (body instanceof FormData) {
    payload = body
  } else if (body !== undefined) {
    headers['content-type'] = 'application/json'
    payload = JSON.stringify(body)
  }
  const res = await fetch(`${BASE}${path}`, { method, headers, body: payload, redirect: 'manual' })
  const set = res.headers.get('set-cookie')
  if (set && set.includes('louver_session=') && !set.includes('Max-Age=0')) {
    cookie = set.split(';')[0]
  }
  const text = await res.text()
  let json = null
  try {
    json = JSON.parse(text)
  } catch {
    /* not json */
  }
  return { status: res.status, text, json, location: res.headers.get('location') }
}
const get = (p) => call('GET', p)
const post = (p, b) => call('POST', p, b)

async function until(what, f, tries = 120, waitMs = 500) {
  for (let i = 0; i < tries; i++) {
    if (await f()) return
    await new Promise((r) => setTimeout(r, waitMs))
  }
  fail(`시간 초과: ${what}`)
}

let ok = false
try {
  server = startServer()
  await waitForServer()

  step(1, 'register')
  const reg = await post('/api/auth/register', { name: '김수강', email: EMAIL, password: PASSWORD })
  if (reg.status !== 200) fail(`register ${reg.status}: ${reg.text}`)
  if (reg.text.includes(PASSWORD)) fail('the password came back in the answer')
  log('가입 완료 ✓', reg.json.email)

  step(2, 'login')
  await post('/api/auth/logout')
  const login = await post('/api/auth/login', { email: EMAIL, password: PASSWORD })
  if (login.status !== 200) fail(`login ${login.status}: ${login.text}`)
  const wrong = await post('/api/auth/login', { email: EMAIL, password: 'nope' })
  if (wrong.status !== 401) fail(`a wrong password answered ${wrong.status}`)
  log('로그인 ✓, 잘못된 비밀번호는 401 ✓')

  step(3, 'a new account is unsubscribed')
  const fresh = (await get('/api/me/subscription')).json
  if (fresh.active !== false || fresh.plan) fail(`not unsubscribed: ${JSON.stringify(fresh)}`)
  // The spend path an unsubscribed account can actually reach: an upload. Its
  // per-file ceiling on the unsubscribed plan is zero bytes.
  const tryUpload = new FormData()
  tryUpload.append('file', new Blob([new Uint8Array(1024)]), 'nope.mp4')
  const denied = await call('POST', '/api/media/upload', tryUpload)
  if (denied.status !== 402) fail(`an unsubscribed upload answered ${denied.status}: ${denied.text}`)
  log('미구독 ✓, 업로드 402 ✓')

  step(4, 'plan catalog')
  const plans = (await get('/api/plans')).json
  const want = { basic: [19900, 1], pro: [39900, 2], business: [59900, 3] }
  for (const [id, [price, streams]] of Object.entries(want)) {
    const p = plans.find((x) => x.id === id)
    if (!p) fail(`요금제 ${id} 가 없습니다`)
    if (p.monthly_price_krw !== price) fail(`${id} 가격 ${p.monthly_price_krw}, 기대 ${price}`)
    if (p.limits.max_concurrent_streams !== streams) {
      fail(`${id} 동시송출 ${p.limits.max_concurrent_streams}, 기대 ${streams}`)
    }
  }
  if (plans.some((p) => p.id === 'none')) fail('미구독 상태가 요금제로 판매되고 있습니다')
  // The storage ceilings a fresh database seeds, and the invariant that a
  // per-file limit can never exceed the whole allowance.
  for (const [id, storage] of Object.entries({ basic: 5, pro: 10, business: 20 })) {
    const p = plans.find((x) => x.id === id)
    const gb = p.limits.max_storage_bytes / 1024 ** 3
    if (gb !== storage) fail(`${id} 저장 한도 ${gb}GB, 기대 ${storage}GB`)
    if (p.limits.max_upload_bytes > p.limits.max_storage_bytes) {
      fail(`${id} 한 파일 한도가 전체 한도보다 큽니다`)
    }
  }
  log('요금제 3종 가격/동시송출/저장한도 일치 ✓')

  step(5, 'checkout (fake PayApp)')
  const checkout = await post('/api/billing/checkout', { plan_id: 'pro', recvphone: '010-1234-5678' })
  if (checkout.status !== 200) fail(`checkout ${checkout.status}: ${checkout.text}`)
  if (checkout.json.amount_krw !== 39900) fail('서버가 요금제 가격을 쓰지 않았습니다')
  const stillNone = (await get('/api/me/subscription')).json
  if (stillNone.active !== false) fail('결제 전에 권한이 생겼습니다')
  log('결제창 URL ✓, 그 자체로는 아무 권한도 주지 않음 ✓')

  step(6, 'verified payment notification')
  const notify = (over = {}) =>
    new URLSearchParams({
      userid: PAY_USERID,
      linkkey: LINKKEY,
      linkval: LINKVAL,
      goodname: '247streams Pro',
      price: '39900',
      recvphone: '01012345678',
      reqdate: '2026-09-28 12:00:00',
      pay_date: '2026-09-28 12:00:05',
      pay_type: 'card',
      pay_state: '4',
      mul_no: '900001',
      rebill_no: 'REL1',
      feedbacktype: 'rebill',
      var1: checkout.json.billing_id,
      ...over,
    }).toString()

  const forged = await fetch(`${BASE}/api/billing/payapp/feedback`, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: notify({ linkkey: 'guessed' }),
  })
  if ((await forged.text()) !== 'FAIL') fail('위조된 결제 알림이 받아들여졌습니다')
  const real = await fetch(`${BASE}/api/billing/payapp/feedback`, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: notify(),
  })
  const said = await real.text()
  if (real.status !== 200 || said !== 'SUCCESS') fail(`알림 응답이 "${said}" (${real.status})`)
  // Twice is once.
  await fetch(`${BASE}/api/billing/payapp/feedback`, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: notify(),
  })
  log('위조 FAIL ✓, 정상 SUCCESS ✓, 중복 알림 1회만 적용 ✓')

  step(7, 'entitlement active')
  const paid = (await get('/api/me/subscription')).json
  if (!paid.active || paid.plan?.id !== 'pro') fail(`활성화되지 않았습니다: ${JSON.stringify(paid)}`)
  const dash = (await get('/api/broadcasts')).json
  if (dash.allowed !== 2) fail(`동시송출 ${dash.allowed}, 기대 2`)
  log('Pro 활성화 ✓, 동시송출 2 ✓')

  step(8, 'YouTube OAuth and provisioning (fake Google)')
  const start = (await get('/api/youtube/oauth/start')).json
  const state = new URL(start.url).searchParams.get('state')
  const back = await get(`/api/youtube/oauth/callback?code=auth-code-1&state=${state}`)
  if (back.status !== 303) fail(`콜백이 ${back.status}`)
  if (!back.location.includes('youtube=connected')) fail(`콜백 이동: ${back.location}`)
  const accounts = (await get('/api/youtube/accounts')).json
  if (accounts.length !== 1) fail(`연결된 계정 ${accounts.length}개`)
  if (JSON.stringify(accounts).includes('refresh-1')) fail('토큰이 API 응답에 들어 있습니다')
  const replay = await get(`/api/youtube/oauth/callback?code=auth-code-1&state=${state}`)
  if (replay.location?.includes('youtube=connected')) fail('같은 state 가 두 번 쓰였습니다')
  log('OAuth 연결 ✓, state 재사용 거부 ✓, 토큰 비노출 ✓')

  step(9, 'upload')
  const clip = join(data, 'clip.mp4')
  const made = spawnSync(
    ffmpegPath,
    ['-y', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=1280x720:rate=30:duration=3',
     '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000:duration=3',
     '-c:v', 'libx264', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p', '-g', '60',
     '-c:a', 'aac', '-b:a', '128k', '-ar', '48000', '-ac', '2', '-shortest', clip],
    { encoding: 'utf8' },
  )
  if (made.status !== 0) fail(`테스트 영상을 만들지 못했습니다: ${made.stderr}`)
  const form = new FormData()
  form.append('file', new Blob([readFileSync(clip)]), 'clip.mp4')
  const up = await call('POST', '/api/media/upload', form)
  if (up.status !== 200) fail(`upload ${up.status}: ${up.text}`)
  const mediaId = up.json.id
  await until('업로드 준비 완료', async () => {
    const m = (await get(`/api/media/${mediaId}`)).json
    if (m.state === 'failed') fail(`준비 실패: ${m.last_error}`)
    return m.state === 'ready'
  })
  // The prepared file must have taken the fast path: this clip is already
  // H.264 yuv420p 48kHz stereo, so nothing about it needs re-encoding, and the
  // whole point of this release is that nothing is.
  const prep = serverLog.join('')
  if (!/media [0-9a-f]{8}: mode=direct video=copy audio=copy/.test(prep)) {
    fail(`호환되는 업로드가 빠른 경로를 타지 않았습니다:\n${prep.split('\n').filter((l) => l.includes('media ')).join('\n')}`)
  }
  log('업로드와 준비 ✓ (mode=direct, 아무것도 재인코딩하지 않음)')

  step(10, 'create a YouTube-backed playlist broadcast')
  const created = await post('/api/broadcasts', {
    name: '릴리스 스모크',
    media_ids: [mediaId],
    youtube_account_id: accounts[0].id,
    loop_forever: true,
  })
  if (created.status !== 200) fail(`create ${created.status}: ${created.text}`)
  // `BroadcastDetail` flattens the broadcast, so the fields are at the top.
  if (!created.json?.id) fail(`create 응답 모양이 다릅니다: ${created.text.slice(0, 400)}`)
  const bId = created.json.id
  const firstYoutubeBroadcast = created.json.youtube.broadcast_id
  if (!firstYoutubeBroadcast) fail('YouTube 방송이 만들어지지 않았습니다')
  if (created.text.includes('live')) {
    /* the ingestion address is fine to see; the key is not */
  }
  if (created.text.match(/"stream_key"|"key"\s*:\s*"live"/)) fail('스트림 키가 응답에 있습니다')
  log('방송 생성 + YouTube provisioning ✓', firstYoutubeBroadcast)

  step(11, 'START — FFmpeg publishes over a real RTMP connection')
  const started = await post(`/api/broadcasts/${bId}/start`)
  if (started.status !== 200) fail(`start ${started.status}: ${started.text}`)
  await until('FFmpeg 송출 시작', async () => {
    const b = (await get(`/api/broadcasts/${bId}`)).json
    return b.runtime_state === 'RUNNING'
  })
  await until('RTMP 싱크가 연결을 받음', async () => sinkLog.join('').length > 0, 40, 500)
  log('송출 중 ✓ (로컬 RTMP 싱크가 연결을 받았습니다)')

  step(12, 'STOP — and YouTube is told, once')
  google.lifecycle = 'live'
  const stopped = await post(`/api/broadcasts/${bId}/stop`)
  if (stopped.status !== 200) fail(`stop ${stopped.status}`)
  if (google.transitions.filter((t) => t === 'complete').length !== 1) {
    fail(`complete 전환 ${google.transitions.join(',')}`)
  }
  log('중지 ✓, YouTube complete 1회 ✓')

  step(13, 'restart semantics — a restart never ends the YouTube broadcast')
  await post(`/api/broadcasts/${bId}/start`)
  await until('다시 송출', async () => {
    const b = (await get(`/api/broadcasts/${bId}`)).json
    return b.runtime_state === 'RUNNING'
  })
  const renewed = (await get(`/api/broadcasts/${bId}`)).json.youtube.broadcast_id
  if (renewed === firstYoutubeBroadcast) fail('끝난 YouTube 방송을 다시 쓰려고 했습니다')
  google.lifecycle = 'live'
  const completesBefore = google.transitions.filter((t) => t === 'complete').length
  const restarted = await post(`/api/broadcasts/${bId}/restart`)
  if (restarted.status !== 200) fail(`restart ${restarted.status}: ${restarted.text}`)
  await until('재시작 후 송출', async () => {
    const b = (await get(`/api/broadcasts/${bId}`)).json
    return b.runtime_state === 'RUNNING'
  })
  const after = (await get(`/api/broadcasts/${bId}`)).json
  if (google.transitions.filter((t) => t === 'complete').length !== completesBefore) {
    fail('재시작이 YouTube 방송을 종료시켰습니다')
  }
  if (after.youtube.broadcast_id !== renewed) fail('재시작이 YouTube 방송을 바꿨습니다')
  if (after.desired_state !== 'running') fail('재시작 후 의도가 stopped 입니다')
  log('종료된 방송은 새로 발급 ✓, 재시작은 complete 하지 않음 ✓')

  step(17, 'process restart and recovery')
  server.kill('SIGKILL')
  await new Promise((r) => setTimeout(r, 800))
  const completesBeforeBoot = google.transitions.filter((t) => t === 'complete').length
  server = startServer()
  await waitForServer()
  await until('복구된 방송이 다시 송출', async () => {
    const b = (await get(`/api/broadcasts/${bId}`)).json
    return b.runtime_state === 'RUNNING'
  })
  if (google.transitions.filter((t) => t === 'complete').length !== completesBeforeBoot) {
    fail('서버 재시작이 YouTube 방송을 종료시켰습니다')
  }
  log('서버를 죽였다 살려도 방송이 돌아옴 ✓, YouTube 는 건드리지 않음 ✓')

  step(18, 'the admin console: shut to members, opened from the shell')
  const meNow = (await get('/api/me')).json
  if (meNow.role !== 'user') fail(`새 계정의 role 이 ${meNow.role} 입니다`)
  const ADMIN_ROUTES = [
    ['GET', '/api/admin/dashboard'],
    ['GET', '/api/admin/revenue'],
    ['GET', '/api/admin/users'],
    ['GET', `/api/admin/users/${meNow.id}`],
    ['GET', `/api/admin/users/${meNow.id}/payments`],
    ['GET', '/api/admin/broadcasts'],
    ['GET', '/api/admin/billing'],
    ['GET', '/api/admin/system'],
    ['GET', '/api/admin/audit'],
    ['POST', `/api/admin/users/${meNow.id}/disabled`],
    ['POST', `/api/admin/broadcasts/${bId}/stop`],
  ]
  // Nobody at all.
  for (const [method, path] of ADMIN_ROUTES) {
    const r = await fetch(`${BASE}${path}`, {
      method,
      headers: { 'content-type': 'application/json' },
      body: method === 'POST' ? '{}' : undefined,
    })
    if (r.status !== 401) fail(`로그인 없이 ${method} ${path} 가 ${r.status}`)
  }
  // A paying member, who is not an operator.
  for (const [method, path] of ADMIN_ROUTES) {
    const r = await call(method, path, method === 'POST' ? {} : undefined)
    if (r.status !== 403) fail(`일반 사용자가 ${method} ${path} 로 ${r.status} 를 받았습니다`)
  }
  log('비로그인 401 ✓, 일반 사용자 403 ✓ (11개 경로)')

  const memberCookie = cookie
  cookie = ''
  const opReg = await post('/api/auth/register', {
    name: '운영자',
    email: ADMIN_EMAIL,
    password: PASSWORD,
  })
  if (opReg.status !== 200) fail(`운영자 가입 ${opReg.status}`)
  // Registering does not make an operator, and neither does any request.
  if ((await get('/api/admin/dashboard')).status !== 403) fail('가입만으로 관리자가 됐습니다')
  const granted = spawnSync('./target/debug/louver-server', ['--set-admin', ADMIN_EMAIL], {
    env,
    encoding: 'utf8',
  })
  if (granted.status !== 0) fail(`--set-admin 실패: ${granted.stderr}`)
  const listed = spawnSync('./target/debug/louver-server', ['--list-admins'], { env, encoding: 'utf8' })
  if (!listed.stdout.includes(ADMIN_EMAIL)) fail('--list-admins 에 관리자가 없습니다')
  log('셸로만 관리자 지정 ✓')

  step(19, 'the dashboard adds up to what actually happened')
  const board = await get('/api/admin/dashboard')
  if (board.status !== 200) fail(`dashboard ${board.status}: ${board.text}`)
  const b0 = board.json
  if (b0.users.total !== 2) fail(`회원 ${b0.users.total}명, 기대 2`)
  // One verified payment of ₩39,900, notified twice on purpose in step 6.
  if (b0.revenue.all_time !== 39900) fail(`누적 매출 ${b0.revenue.all_time}, 기대 39900`)
  if (b0.revenue.today !== 39900) fail(`오늘 매출 ${b0.revenue.today}, 기대 39900`)
  if (b0.mrr_krw !== 39900) fail(`MRR ${b0.mrr_krw}, 기대 39900`)
  if (b0.subscriptions.paid_total !== 1) fail(`유료 ${b0.subscriptions.paid_total}명, 기대 1`)
  if (b0.broadcasts.running !== 1) fail(`송출 중 ${b0.broadcasts.running}, 기대 1`)
  if (b0.youtube_accounts !== 1) fail(`YouTube 연결 ${b0.youtube_accounts}, 기대 1`)
  const report = await get('/api/admin/revenue?from=2020-01-01&to=2099-12-31&grain=month')
  if (report.status !== 200) fail(`revenue ${report.status}`)
  const buckets = report.json.buckets.reduce((n, x) => n + x.krw, 0)
  if (buckets !== 39900) fail(`기간 합계 ${buckets}, 기대 39900`)
  if (report.json.buckets.reduce((n, x) => n + x.new_krw, 0) !== 39900) {
    fail('첫 결제가 신규로 계산되지 않았습니다')
  }
  log('대시보드 숫자 = 실제로 일어난 일 ✓, 중복 알림 1회만 ✓')

  step(20, 'force stop is the ordinary stop, and it is written down')
  const adminList = (await get('/api/admin/broadcasts?filter=running')).json
  const mine = adminList.find((x) => x.id === bId)
  if (!mine) fail('관리자 화면에 송출 중인 방송이 없습니다')
  if (!mine.worker_alive) fail('워커가 살아 있는데 죽은 것으로 보입니다')
  google.lifecycle = 'live'
  const completesBeforeForce = google.transitions.filter((t) => t === 'complete').length
  const forced = await post(`/api/admin/broadcasts/${bId}/stop`, { note: '고객 요청 (릴리스 점검)' })
  if (forced.status !== 200) fail(`force stop ${forced.status}: ${forced.text}`)
  if (forced.json.desired_state !== 'stopped') fail('강제 종료 후에도 의도가 running 입니다')
  if (google.transitions.filter((t) => t === 'complete').length !== completesBeforeForce + 1) {
    fail('강제 종료가 YouTube 방송을 종료하지 않았습니다')
  }
  await new Promise((r) => setTimeout(r, 2000))
  const afterForce = (await get(`/api/admin/broadcasts`)).json.find((x) => x.id === bId)
  if (afterForce.desired_state !== 'stopped' || afterForce.worker_alive) {
    fail('강제 종료한 방송을 watchdog 이 되살렸습니다')
  }
  const audit = (await get('/api/admin/audit')).json
  const line = audit.find((a) => a.action === 'admin.broadcast.force_stop' && a.target_id === bId)
  if (!line) fail('감사 로그에 강제 종료가 없습니다')
  if (line.admin_email !== ADMIN_EMAIL) fail(`감사 로그의 관리자가 ${line.admin_email}`)
  if (line.note !== '고객 요청 (릴리스 점검)') fail('사유가 남지 않았습니다')
  log('강제 종료 = 일반 종료 ✓, watchdog 안 살림 ✓, 감사 로그 ✓')

  step('☑', '관리자에게도 비밀정보는 보이지 않는다')
  const surfaces = [
    '/api/admin/dashboard',
    '/api/admin/users',
    `/api/admin/users/${meNow.id}`,
    '/api/admin/broadcasts',
    '/api/admin/billing',
    '/api/admin/system',
    '/api/admin/audit',
  ]
  for (const path of surfaces) {
    const body = (await get(path)).text
    for (const secret of [LINKKEY, LINKVAL, env.YOUTUBE_CLIENT_SECRET, env.LOUVER_MASTER_KEY, 'refresh-1', PASSWORD]) {
      if (body.includes(secret)) fail(`${path} 응답에 비밀정보가 있습니다`)
    }
  }
  log('7개 관리자 응답에 비밀정보 없음 ✓')

  // Back to the student, and back on air: the cancellation policy below is
  // about what happens to a broadcast that is running when somebody cancels.
  cookie = memberCookie
  const backOn = await post(`/api/broadcasts/${bId}/start`)
  if (backOn.status !== 200) fail(`다시 시작 ${backOn.status}: ${backOn.text}`)
  await until('다시 송출', async () => {
    const b = (await get(`/api/broadcasts/${bId}`)).json
    return b.runtime_state === 'RUNNING'
  })

  step(14, 'cancel the subscription — while a broadcast is on air')
  // Deliberately not stopped first: the policy is that confirming a
  // cancellation ends whatever is running.
  const onAir = (await get(`/api/broadcasts/${bId}`)).json
  if (onAir.desired_state !== 'running') fail('취소 전에 방송이 돌고 있지 않습니다')
  google.lifecycle = 'live'
  const completesBeforeCancel = google.transitions.filter((t) => t === 'complete').length
  const cancelled = await post('/api/billing/cancel')
  if (cancelled.status !== 200) fail(`cancel ${cancelled.status}: ${cancelled.text}`)
  if (cancelled.json.status !== 'cancelled') fail(`상태 ${cancelled.json.status}`)

  // Before the answer came back, everything had to be consistent already.
  const afterCancel = (await get(`/api/broadcasts/${bId}`)).json
  if (afterCancel.desired_state !== 'stopped') fail('해지 후에도 방송이 running 입니다')
  if (afterCancel.runtime_state !== 'STOPPED') fail(`해지 후 runtime=${afterCancel.runtime_state}`)
  if (google.transitions.filter((t) => t === 'complete').length !== completesBeforeCancel + 1) {
    fail('해지가 YouTube 방송을 종료하지 않았습니다')
  }
  if ((await get('/api/broadcasts')).json.active !== 0) fail('해지 후에도 슬롯이 잡혀 있습니다')
  log('해지 ✓, 송출 중이던 방송 종료 ✓, YouTube complete ✓')

  // And nothing brings it back.
  await new Promise((r) => setTimeout(r, 2000))
  const later = (await get(`/api/broadcasts/${bId}`)).json
  if (later.desired_state !== 'stopped' || later.runtime_state === 'RUNNING') {
    fail('해지된 방송이 되살아났습니다')
  }
  log('watchdog 이 되살리지 않음 ✓')

  step(15, 'entitlement is gone immediately')
  const gone = (await get('/api/me/subscription')).json
  if (gone.active !== false || gone.plan) fail(`권한이 남았습니다: ${JSON.stringify(gone)}`)
  if ((await get('/api/broadcasts')).json.allowed !== 0) fail('동시송출이 0이 아닙니다')
  log('즉시 미구독 ✓')

  step(16, 'a cancelled account cannot get back on air')
  for (const [what, path] of [
    ['start', `/api/broadcasts/${bId}/start`],
    ['restart', `/api/broadcasts/${bId}/restart`],
  ]) {
    const r = await post(path)
    if (r.status !== 402) fail(`${what} 가 ${r.status} 로 답했습니다`)
  }
  const create = await post('/api/broadcasts', {
    name: '또 하나',
    media_ids: [mediaId],
    youtube_account_id: accounts[0].id,
  })
  if (create.status !== 402) fail(`해지 후 방송 생성이 ${create.status}`)
  log('start / restart / create 모두 402 ✓')

  step('☑', '비밀정보가 로그에 남지 않았는지')
  const logs = serverLog.join('')
  for (const secret of [LINKKEY, LINKVAL, env.YOUTUBE_CLIENT_SECRET, env.LOUVER_MASTER_KEY, 'refresh-1', PASSWORD]) {
    if (logs.includes(secret)) fail(`비밀정보가 로그에 있습니다: ${secret.slice(0, 6)}…`)
  }
  log('로그에 비밀정보 없음 ✓')
  ok = true
} catch (e) {
  console.error('\n[release] FAILED:', e.message)
  console.error(serverLog.join('').split('\n').slice(-40).join('\n'))
} finally {
  if (server) server.kill('SIGKILL')
  sink.kill('SIGKILL')
  fakePay.close()
  fakeGoogle.close()
  rmSync(data, { recursive: true, force: true })
  console.log(ok ? '\n[release] ALL PASS' : '\n[release] FAILED')
  process.exit(ok ? 0 : 1)
}
