#!/usr/bin/env node
/**
 * The screens a student will use, at phone size, in a real browser.
 *
 * Checks the two things that actually break a phone layout and that no unit test
 * can see: a page wider than the screen, and a control pushed off it. Every
 * viewport is walked through sign-up, the price list, the checkout modal, the
 * dashboard, the YouTube page and the billing panel.
 *
 *   node scripts/mobile-smoke.mjs
 *
 * Needs a built server and `apps/web/dist`. Nothing here reaches PayApp or
 * Google: PayApp is a local fake and YouTube is simply not configured, which is
 * a supported deployment and the state the page has to handle anyway.
 */
import { createServer } from 'node:http'
import { spawn, spawnSync } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const { chromium } = await import('playwright')

const PORT = 8094
const PAY_PORT = 8095
const BASE = `http://127.0.0.1:${PORT}`
const PASSWORD = 'correct-horse-battery'
const log = (...a) => console.log('[mobile]', ...a)
const fail = (m) => {
  throw new Error(m)
}

/// iPhone 13 mini, iPhone 15 Pro Max, and a small Android — the narrowest
/// widths a student is likely to hold, plus a desktop for comparison.
const VIEWPORTS = [
  { name: 'iPhone SE', width: 375, height: 667 },
  { name: 'iPhone 15', width: 393, height: 852 },
  { name: 'Galaxy A', width: 360, height: 800 },
  { name: 'desktop', width: 1280, height: 800 },
]

const fake = createServer((req, res) => {
  let body = ''
  req.on('data', (c) => (body += c))
  req.on('end', () => {
    res.writeHead(200, { 'Content-Type': 'text/plain' })
    res.end(`state=1&errno=00000&rebill_no=MOB1&payurl=${encodeURIComponent(`${BASE}/billing/complete`)}`)
  })
})
await new Promise((r) => fake.listen(PAY_PORT, '127.0.0.1', r))

const data = mkdtempSync(join(tmpdir(), 'mobile-smoke-'))
const env = {
  ...process.env,
  LOUVER_DATA_DIR: data,
  LOUVER_MASTER_KEY: 'ef'.repeat(32),
  LOUVER_WEB_DIR: 'apps/web/dist',
  LOUVER_FFMPEG_DIR: 'apps/desktop/src-tauri/binaries',
  LOUVER_BIND: `127.0.0.1:${PORT}`,
  LOUVER_INSECURE_COOKIES: '1',
  LOUVER_DEPLOYMENT: 'local',
  LOUVER_PUBLIC_URL: BASE,
  PAYAPP_USERID: 'mobile-merchant',
  PAYAPP_LINKKEY: 'mobile-link-key',
  PAYAPP_LINKVAL: 'mobile-link-val',
  PAYAPP_API_URL: `http://127.0.0.1:${PAY_PORT}/oapi/apiLoad.html`,
}
const server = spawn('./target/debug/louver-server', [], { env, stdio: ['ignore', 'pipe', 'pipe'] })
const serverLog = []
for (const s of [server.stdout, server.stderr]) s.on('data', (c) => serverLog.push(c.toString()))

let ok = false
let browser = null
try {
  for (let i = 0; i < 80; i++) {
    try {
      await fetch(`${BASE}/health`)
      break
    } catch {
      await new Promise((r) => setTimeout(r, 250))
    }
  }

  browser = await chromium.launch(
    process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {},
  )

  for (const vp of VIEWPORTS) {
    const ctx = await browser.newContext({ viewport: { width: vp.width, height: vp.height } })
    const page = await ctx.newPage()
    const problems = []

    /** Nothing may be wider than the screen, and no control may hang off it. */
    const measure = async (where) => {
      const overflow = await page.evaluate(() => ({
        doc: document.documentElement.scrollWidth,
        seen: window.innerWidth,
      }))
      if (overflow.doc > overflow.seen + 1) {
        problems.push(`${where}: 가로 스크롤 (문서 ${overflow.doc}px > 화면 ${overflow.seen}px)`)
      }
      const offscreen = await page.evaluate(() => {
        const out = []
        for (const el of document.querySelectorAll('button, a, input, select')) {
          const r = el.getBoundingClientRect()
          if (r.width === 0 && r.height === 0) continue
          if (r.right > window.innerWidth + 1 || r.left < -1) {
            out.push((el.textContent || el.getAttribute('aria-label') || el.tagName).trim().slice(0, 24))
          }
        }
        return out
      })
      if (offscreen.length) problems.push(`${where}: 화면을 벗어난 컨트롤 ${offscreen.join(' / ')}`)
    }

    await page.goto(BASE)
    await page.getByRole('button', { name: /회원가입/ }).waitFor({ timeout: 15000 })
    await measure('로그인')

    await page.getByRole('button', { name: /회원가입/ }).click()
    await measure('회원가입')

    const email = `m-${vp.width}-${Date.now()}@example.com`
    await page.getByLabel('이름').fill('김수강')
    await page.getByLabel('이메일').fill(email)
    await page.getByLabel('비밀번호', { exact: true }).fill(PASSWORD)
    await page.getByLabel('비밀번호 확인').fill(PASSWORD)
    await page.getByLabel(/동의합니다/).check()
    await measure('회원가입(입력 후)')
    await page.getByRole('button', { name: '무료로 시작하기' }).click()
    await page.getByRole('button', { name: '로그아웃' }).waitFor({ timeout: 15000 })
    await measure('대시보드(미구독)')

    // Every tab the header offers.
    for (const tab of ['요금제', '미디어', 'YouTube', '서버']) {
      const button = page.getByRole('button', { name: tab, exact: true })
      if ((await button.count()) === 0) continue
      await button.first().click()
      await page.waitForTimeout(250)
      await measure(tab)
    }

    // The checkout modal, which is the narrowest thing on the site.
    await page.getByRole('button', { name: '요금제', exact: true }).first().click()
    const buy = page.getByRole('button', { name: /결제하기|시작하기/ }).first()
    if ((await buy.count()) > 0) {
      await buy.click()
      await page.waitForTimeout(400)
      await measure('결제 모달')
      const phone = page.getByLabel(/휴대폰/)
      if ((await phone.count()) > 0) {
        await phone.first().fill('010-1234-5678')
        await measure('결제 모달(입력 후)')
      }
      await page.keyboard.press('Escape')
    }

    // And the two pages a payment provider requires.
    for (const path of ['/terms', '/privacy']) {
      await page.goto(BASE + path)
      await page.waitForTimeout(200)
      await measure(path)
    }

    // The operator console. Denser than anything a student sees, and the one
    // screen somebody will open from a phone in the middle of an incident.
    // The grant is the same shell command as in production.
    const granted = spawnSync('./target/debug/louver-server', ['--set-admin', email], {
      env,
      encoding: 'utf8',
    })
    if (granted.status !== 0) fail(`--set-admin: ${granted.stderr}`)
    await page.goto(`${BASE}/admin`)
    await page.getByRole('button', { name: '대시보드', exact: true }).waitFor({ timeout: 15000 })
    await measure('/admin')
    for (const s of ['매출', '회원', '방송', '결제', '시스템', '감사 로그']) {
      await page.getByRole('button', { name: s, exact: true }).first().click()
      await page.waitForTimeout(350)
      await measure(`/admin ${s}`)
    }

    // The grant form, at phone width. It is the densest thing in the console —
    // four option rows, two date fields and a confirmation step — and it is
    // opened from the members list, so that is the path it is measured on.
    await page.getByRole('button', { name: '회원', exact: true }).first().click()
    await page.waitForTimeout(400)
    const pick = page.locator('input[type=checkbox]').first()
    if ((await pick.count()) > 0) {
      await pick.check()
      await measure('/admin 회원(선택)')
      await page.getByRole('button', { name: '이용권 지급', exact: true }).click()
      await page.waitForTimeout(350)
      await measure('이용권 지급 모달')
      await page.getByRole('button', { name: '직접 설정', exact: true }).click()
      await page.waitForTimeout(250)
      await measure('이용권 지급 모달(직접 설정)')
      await page.getByRole('button', { name: '기타', exact: true }).click()
      await page.waitForTimeout(250)
      await measure('이용권 지급 모달(사유 입력)')
      await page.getByRole('button', { name: '취소', exact: true }).click()
    }

    await ctx.close()
    if (problems.length) {
      console.error(`[mobile] ${vp.name} (${vp.width}px):`)
      for (const p of problems) console.error(`    ✗ ${p}`)
      fail(`${vp.name} 에서 레이아웃 문제 ${problems.length}건`)
    }
    log(`${vp.name} (${vp.width}×${vp.height}) ✓`)
  }
  ok = true
} catch (e) {
  console.error('[mobile] FAILED:', e.message)
} finally {
  if (browser) await browser.close()
  server.kill('SIGKILL')
  fake.close()
  rmSync(data, { recursive: true, force: true })
  console.log(ok ? '[mobile] ALL PASS' : '[mobile] FAILED')
  process.exit(ok ? 0 : 1)
}
