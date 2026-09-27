#!/usr/bin/env node
/**
 * The PayApp checkout in a real browser, against a fake PayApp.
 *
 * Runs a tiny local server that answers `apiLoad.html` the way PayApp documents,
 * points 247streams at it with `PAYAPP_API_URL`, and then drives the real UI:
 * sign up → price list → checkout → be sent to the "payment" URL → post the
 * notification the way PayApp's server would → see the plan activate.
 *
 * Nothing here reaches `api.payapp.kr`. It creates an account and takes no money.
 *
 *   node scripts/billing-smoke.mjs        # starts everything itself
 *
 * Not part of `npm run verify`: it needs a browser binary and a built server.
 */
import { createServer } from 'node:http'
import { spawn } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const { chromium } = await import('playwright')

const USERID = 'smoke-merchant'
const LINKKEY = 'smoke-link-key'
const LINKVAL = 'smoke-link-val'
const PORT = 8097
const FAKE_PORT = 8098
const BASE = `http://127.0.0.1:${PORT}`
const EMAIL = `pay-${Date.now()}@example.com`
const PASSWORD = 'correct-horse-battery'
const log = (...a) => console.log('[billing]', ...a)

// --- a fake PayApp ---------------------------------------------------------
const registrations = []
const fake = createServer((req, res) => {
  let body = ''
  req.on('data', (c) => (body += c))
  req.on('end', () => {
    const f = Object.fromEntries(new URLSearchParams(body))
    if (f.cmd === 'rebillRegist') {
      registrations.push(f)
      res.writeHead(200, { 'Content-Type': 'text/plain' })
      // URL-encoded key=value, as the documentation describes the reply.
      res.end(
        `state=1&errno=00000&rebill_no=SMOKE1&payurl=${encodeURIComponent(`${BASE}/billing/complete`)}`,
      )
      return
    }
    res.writeHead(200, { 'Content-Type': 'text/plain' })
    res.end('state=1&errno=00000')
  })
})
await new Promise((r) => fake.listen(FAKE_PORT, '127.0.0.1', r))
log(`fake PayApp on :${FAKE_PORT}`)

// --- the real server ------------------------------------------------------
const data = mkdtempSync(join(tmpdir(), 'billing-smoke-'))
const server = spawn('./target/debug/louver-server', [], {
  env: {
    ...process.env,
    LOUVER_DATA_DIR: data,
    LOUVER_MASTER_KEY: 'cd'.repeat(32),
    LOUVER_WEB_DIR: 'apps/web/dist',
    LOUVER_FFMPEG_DIR: 'apps/desktop/src-tauri/binaries',
    LOUVER_BIND: `127.0.0.1:${PORT}`,
    LOUVER_INSECURE_COOKIES: '1',
    LOUVER_DEPLOYMENT: 'local',
    PAYAPP_USERID: USERID,
    PAYAPP_LINKKEY: LINKKEY,
    PAYAPP_LINKVAL: LINKVAL,
    PAYAPP_API_URL: `http://127.0.0.1:${FAKE_PORT}/oapi/apiLoad.html`,
    LOUVER_PUBLIC_URL: BASE,
  },
  stdio: ['ignore', 'pipe', 'pipe'],
})
const serverLog = []
for (const stream of [server.stdout, server.stderr]) {
  stream.on('data', (c) => serverLog.push(c.toString()))
}

let ok = false
const fail = (m) => {
  throw new Error(m)
}
try {
  for (let i = 0; i < 60; i++) {
    try {
      await fetch(`${BASE}/health`)
      break
    } catch {
      await new Promise((r) => setTimeout(r, 250))
    }
  }
  log('server up')

  const browser = await chromium.launch(
    process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {},
  )
  const ctx = await browser.newContext()
  const page = await ctx.newPage()
  await page.goto(BASE)

  // Sign up.
  await page.getByRole('button', { name: '계정이 없으신가요? 회원가입' }).click()
  await page.getByLabel('이름').fill('홍길동')
  await page.getByLabel('이메일').fill(EMAIL)
  await page.getByLabel('비밀번호', { exact: true }).fill(PASSWORD)
  await page.getByLabel('비밀번호 확인').fill(PASSWORD)
  await page.getByLabel(/동의합니다/).check()
  await page.getByRole('button', { name: '무료로 시작하기' }).click()
  await page.getByRole('button', { name: '로그아웃' }).waitFor({ timeout: 15000 })
  log('signed up, unsubscribed ✓')

  // Checkout.
  await page.getByRole('button', { name: '요금제', exact: true }).click()
  await page.getByTestId('pricing-grid').waitFor({ timeout: 10000 })
  await page.getByRole('button', { name: 'Pro 시작하기' }).click()
  const summary = await page.getByTestId('checkout-summary').textContent()
  if (!summary.includes('₩39,900')) fail(`checkout summary: ${summary}`)
  await page.getByLabel('휴대폰 번호').fill('010-1234-5678')
  await page.getByRole('button', { name: '정기결제 시작' }).click()

  // The browser is sent to PayApp's `payurl`, which this fake points back at our
  // own completion page — the same navigation a real payment produces.
  await page.waitForURL(/\/billing\/complete/, { timeout: 15000 })
  log('sent to the payment URL ✓')

  // §11: the return URL alone proves nothing. The page must not claim success.
  await page.getByTestId('billing-complete-waiting').waitFor({ timeout: 10000 })
  const sub = await (await ctx.request.get(`${BASE}/api/me/subscription`)).json()
  if (sub.active !== false) fail(`the return URL activated a plan: ${JSON.stringify(sub)}`)
  log('return URL alone activates nothing ✓', `(status=${sub.status})`)

  // The registration PayApp received.
  if (registrations.length !== 1) fail(`expected one rebillRegist, saw ${registrations.length}`)
  const reg = registrations[0]
  for (const [k, v] of [
    ['cmd', 'rebillRegist'],
    ['userid', USERID],
    ['goodname', '247streams Pro'],
    ['goodprice', '39900'],
    ['recvphone', '01012345678'],
    ['rebillCycleType', 'Month'],
    ['openpaytype', 'card'],
  ]) {
    if (reg[k] !== v) fail(`rebillRegist ${k}=${reg[k]}, expected ${v}`)
  }
  if (!/^\d{4}-\d{2}-\d{2}$/.test(reg.rebillExpire)) fail(`rebillExpire=${reg.rebillExpire}`)
  if (reg.linkkey || reg.linkval) fail('rebillRegist must not carry link keys')
  if (reg.var1.includes('@')) fail(`var1 carries an email: ${reg.var1}`)
  log('rebillRegist fields ✓', `cycleMonth=${reg.rebillCycleMonth} expire=${reg.rebillExpire}`)

  // Now the notification, exactly as PayApp's server would post it.
  const notify = async (over = {}) =>
    fetch(`${BASE}/api/billing/payapp/feedback`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({
        userid: USERID,
        linkkey: LINKKEY,
        linkval: LINKVAL,
        goodname: '247streams Pro',
        price: '39900',
        recvphone: '01012345678',
        pay_date: '2026-09-27 12:00:05',
        pay_type: 'card',
        pay_state: '4',
        mul_no: '700001',
        rebill_no: 'SMOKE1',
        var1: reg.var1,
        var2: 'pro',
        ...over,
      }),
    })

  // A forgery first.
  const forged = await notify({ linkkey: 'wrong' })
  if (forged.status !== 400 || (await forged.text()) !== 'FAIL') fail('a forged notification was accepted')
  const stillNot = await (await ctx.request.get(`${BASE}/api/me/subscription`)).json()
  if (stillNot.active !== false) fail('a forged notification granted a plan')
  log('forged notification rejected with FAIL ✓')

  // The real one.
  const res = await notify()
  const text = await res.text()
  if (res.status !== 200 || text !== 'SUCCESS') fail(`feedback answered ${res.status} ${text}`)
  log('notification accepted with exactly SUCCESS ✓')

  // Ten times over, to prove idempotency end to end.
  for (let i = 0; i < 9; i++) {
    const again = await notify()
    if ((await again.text()) !== 'SUCCESS') fail(`repeat ${i} answered non-SUCCESS`)
  }
  const after = await (await ctx.request.get(`${BASE}/api/me/subscription`)).json()
  if (after.active !== true || after.plan?.id !== 'pro') fail(`not activated: ${JSON.stringify(after)}`)
  log('ten notifications, one activation ✓', `plan=${after.plan.id}`)

  // The completion page now says so, and the dashboard agrees.
  await page.reload()
  await page.getByTestId('billing-complete-done').waitFor({ timeout: 15000 })
  await page.getByRole('button', { name: '대시보드로' }).click()
  await page.getByTestId('slots').waitFor({ timeout: 10000 })
  const slots = (await page.getByTestId('slots').textContent()).trim()
  if (slots !== '0 / 2') fail(`slots=${slots}, expected 0 / 2`)
  log('completion page and dashboard agree ✓', `slots=${slots}`)

  // Cancelling stops the next charge and keeps the paid period.
  await page.getByRole('button', { name: '요금제', exact: true }).click()
  await page.getByRole('button', { name: '구독 해지' }).click()
  await page.getByRole('button', { name: '해지하기' }).click()
  await page.getByTestId('cancel-scheduled').waitFor({ timeout: 10000 })
  const kept = await (await ctx.request.get(`${BASE}/api/me/subscription`)).json()
  if (kept.active !== true) fail('cancelling removed a paid-for entitlement')
  log('cancelled; entitlement kept ✓', `status=${kept.status}`)

  // No credential in the server log.
  const logs = serverLog.join('')
  for (const secret of [LINKKEY, LINKVAL]) {
    if (logs.includes(secret)) fail(`a credential reached the log: ${secret}`)
  }
  log('no credential in the server log ✓')

  await browser.close()
  ok = true
} finally {
  server.kill('SIGKILL')
  fake.close()
  rmSync(data, { recursive: true, force: true })
}
log(ok ? 'ALL PASS' : 'FAILED')
process.exit(ok ? 0 : 1)
