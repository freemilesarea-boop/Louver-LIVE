#!/usr/bin/env node
/**
 * Drive the real web UI in a real browser, exactly as a person would:
 * register → upload → wait until the video is usable → save a destination with
 * a stream key → create a broadcast → START → watch it go live → close the
 * browser entirely → sign back in and find it still running → STOP.
 *
 * Not part of `npm run verify`: it needs a browser binary and a server that is
 * already running, neither of which belongs in a unit-test step.
 *
 *   npm run cloud:dev                       # in one terminal
 *   npx playwright install chromium         # once
 *   node scripts/browser-smoke.mjs --video ./clip.mp4
 *
 * By default it publishes to a local RTMP sink (`npm run rc:sink`), not to
 * YouTube. Pass --rtmp/--key to point it somewhere else.
 */
let chromium
try {
  ;({ chromium } = await import('playwright'))
} catch {
  console.error('playwright가 없습니다:  npm i -D playwright && npx playwright install chromium')
  process.exit(1)
}

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`)
  return i === -1 ? fallback : process.argv[i + 1]
}

const BASE = arg('base', 'http://127.0.0.1:8080')
const EMAIL = arg('email', `smoke-${Date.now()}@example.com`)
const PASSWORD = arg('password', 'correct-horse-battery')
const VIDEO = arg('video', process.env.VIDEO)
const RTMP = arg('rtmp', 'rtmp://127.0.0.1:1935/live')
const KEY = arg('key', 'rc-test')

if (!VIDEO) {
  console.error('사용법: node scripts/browser-smoke.mjs --video ./clip.mp4')
  process.exit(1)
}

const log = (...a) => console.log('[browser]', ...a)

// Fail here, with a sentence, rather than after thirty seconds of a blank page.
try {
  const r = await fetch(`${BASE}/health`)
  const h = await r.json()
  console.log(`[browser] 서버: ${h.status}, ${h.deployment}`)
} catch {
  console.error(`[browser] ${BASE} 에 연결할 수 없습니다. 먼저 \`npm run cloud:dev\` 를 실행하세요.`)
  process.exit(1)
}

const browser = await chromium.launch(
  process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {},
)
const page = await browser.newPage()
const problems = []
page.on('console', (m) => {
  // The session probe on a cold load answers 401 until someone signs in. That
  // is the protocol working, not a fault.
  const text = m.text()
  if (m.type() === 'error' && !text.includes('401')) problems.push(`console: ${text}`)
})
page.on('pageerror', (e) => problems.push(`pageerror: ${e.message}`))

await page.goto(BASE)
log('title:', await page.title())

const banner = page.getByTestId('deployment-banner')
await banner.waitFor({ timeout: 10000 })
log('banner:', (await banner.textContent()).trim().slice(0, 80))

// --- register --------------------------------------------------------------
await page.getByRole('button', { name: '계정이 없으신가요? 만들기' }).click()
await page.getByLabel('이메일').fill(EMAIL)
await page.getByLabel('비밀번호').fill(PASSWORD)
await page.getByRole('button', { name: '계정 만들기' }).click()
await page.getByRole('button', { name: '로그아웃' }).waitFor({ timeout: 15000 })
log('registered and signed in as', EMAIL)

// --- upload ----------------------------------------------------------------
await page.getByRole('button', { name: '영상' }).click()
await page.setInputFiles('input[type=file]', VIDEO)
log('uploading', VIDEO)
await page.getByText('사용 가능').waitFor({ timeout: 180000 })
log('media ready')

// --- destination -----------------------------------------------------------
await page.getByRole('button', { name: '송출 대상' }).click()
await page.getByRole('button', { name: '대상 추가' }).click()
await page.getByLabel('스트림 키').fill(KEY)
await page.getByLabel('서버 주소').fill(RTMP)
await page.getByRole('button', { name: '저장' }).click()
await page.getByTestId('destination-row').waitFor({ timeout: 10000 })
const masked = await page.getByTestId('masked-key').textContent()
log('destination saved, key shown as', masked)
if ((await page.content()).includes(KEY)) problems.push('the stream key is in the page')

// --- broadcast -------------------------------------------------------------
await page.getByRole('button', { name: '방송', exact: true }).click()
await page.getByRole('button', { name: '방송 만들기' }).click()
await page.getByLabel('방송 이름').fill('브라우저 테스트')
await page.getByRole('button', { name: '만들기', exact: true }).click()
await page.getByTestId('broadcast-row').waitFor({ timeout: 10000 })
log('broadcast created')

await page.getByRole('button', { name: '시작' }).click()
await page.getByText('송출 중').waitFor({ timeout: 30000 })
log('LIVE:', (await page.getByTestId('broadcast-row').textContent()).trim().replace(/\s+/g, ' '))
log('slots:', await page.getByTestId('slots').textContent())

// The SSE stream must keep the row live without a reload.
await page.waitForTimeout(6000)
log('after 6s, still:', (await page.getByTestId('broadcast-row').textContent()).trim().replace(/\s+/g, ' '))

// --- metrics ---------------------------------------------------------------
await page.getByRole('button', { name: '서버 상태' }).click()
await page.getByTestId('metrics-row').waitFor({ timeout: 15000 })
log('metrics:', (await page.getByTestId('metrics-row').textContent()).trim().replace(/\s+/g, ' '))

// --- close the browser entirely, reopen, and see it still running ----------
await page.context().close()
const second = await browser.newContext()
const again = await second.newPage()
await again.goto(BASE)
await again.getByRole('button', { name: '계정이 없으신가요? 만들기' }).waitFor({ timeout: 10000 })
await again.getByLabel('이메일').fill(EMAIL)
await again.getByLabel('비밀번호').fill(PASSWORD)
await again.getByRole('button', { name: '로그인' }).click()
await again.getByText('송출 중').waitFor({ timeout: 20000 })
log('after closing the browser and signing back in: still 송출 중')
log('slots:', await again.getByTestId('slots').textContent())

// --- stop ------------------------------------------------------------------
await again.getByRole('button', { name: '중지' }).click()
await again.getByTestId('broadcast-row').getByText('중지됨').first().waitFor({ timeout: 20000 })
log('stopped')

if ((await again.content()).includes(KEY)) problems.push('the stream key is in the page after reload')
await browser.close()

if (problems.length) {
  console.error('[browser] PROBLEMS:', problems)
  process.exit(1)
}
console.log('[browser] OK')
