#!/usr/bin/env node
/**
 * The plan lifecycle in a real browser: sign up, be unsubscribed, read the price
 * list, press a plan's button, and confirm the server still says unsubscribed.
 *
 * The last part is the point. A button that quietly activated a plan would be a
 * way to get a paid entitlement for free, so this asks the server afterwards
 * rather than trusting the page, and then tries every route somebody would reach
 * for to grant themselves one.
 *
 * Not part of `npm run verify`: it needs a browser binary and a running server.
 *
 *   LOUVER_DATA_DIR=$(mktemp -d) LOUVER_MASTER_KEY=$(openssl rand -hex 32) \
 *     LOUVER_WEB_DIR=apps/web/dist LOUVER_BIND=127.0.0.1:8099 \
 *     LOUVER_INSECURE_COOKIES=1 ./target/debug/louver-server &
 *   BASE=http://127.0.0.1:8099 node scripts/pricing-smoke.mjs
 *
 * Always against a throwaway data directory: it creates an account.
 */
const { chromium } = await import('playwright')
const BASE = process.env.BASE ?? 'http://127.0.0.1:8099'
const EMAIL = `plans-${Date.now()}@example.com`
const PASSWORD = 'correct-horse-battery'
const log = (...a) => console.log('[plans]', ...a)

// The price list is public: no session at all.
const anon = await (await fetch(`${BASE}/api/plans`)).json()
log('GET /api/plans (no session):', anon.map((p) => `${p.id}=${p.monthly_price_krw}/${p.limits.max_concurrent_streams}`).join(' '))
if (anon.length !== 3) throw new Error(`expected 3 plans, got ${anon.length}`)
for (const [i, [id, price, streams]] of [['basic', 19900, 1], ['pro', 39900, 2], ['business', 59900, 3]].entries()) {
  if (anon[i].id !== id) throw new Error(`plan ${i} is ${anon[i].id}, expected ${id}`)
  if (anon[i].monthly_price_krw !== price) throw new Error(`${id} costs ${anon[i].monthly_price_krw}`)
  if (anon[i].limits.max_concurrent_streams !== streams) throw new Error(`${id} allows ${anon[i].limits.max_concurrent_streams}`)
}
if (anon.some((p) => p.id === 'none')) throw new Error('the unsubscribed plan must not be on sale')

const browser = await chromium.launch(process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {})
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
log('signed up ✓')

// Unsubscribed: the banner is there and the plan reads 요금제 없음.
const banner = page.getByTestId('no-subscription-banner')
await banner.waitFor({ timeout: 10000 })
if (!(await banner.textContent()).includes('현재 활성화된 요금제가 없습니다')) throw new Error('banner text')
if ((await page.getByTestId('plan-label').textContent()).trim() !== '요금제 없음') throw new Error('plan label')
if ((await page.getByTestId('slots').textContent()).trim() !== '0 / 0') throw new Error('slots')
log('dashboard says unsubscribed ✓')

// Refusing to *create* a broadcast needs a real video and destination to reach
// the entitlement check, so that case lives in `apps/server/tests/http_api.rs`
// (`an_unsubscribed_account_can_look_around_and_cannot_spend_anything`) rather
// than here, where uploading one would be the bulk of the script.

// The CTA takes us to the price list.
await page.getByRole('button', { name: '요금제 보기' }).click()
await page.getByTestId('pricing-grid').waitFor({ timeout: 10000 })
const cards = await page.getByTestId('plan-card').all()
if (cards.length !== 3) throw new Error(`${cards.length} cards`)
for (const [id, text] of [['basic', '₩19,900'], ['pro', '₩39,900'], ['business', '₩59,900']]) {
  const card = page.locator(`[data-plan="${id}"]`)
  const body = await card.textContent()
  if (!body.includes(text)) throw new Error(`${id} card missing ${text}: ${body}`)
  if (!body.includes('24/7 클라우드 송출')) throw new Error(`${id} card missing the feature list`)
}
if (!(await page.locator('[data-plan="pro"]').textContent()).includes('추천')) throw new Error('Pro is not marked')
log('pricing page: 3 cards, correct prices, features, Pro recommended ✓')

// Pressing a button changes nothing on the server.
await page.getByRole('button', { name: 'Pro 시작하기' }).click()
await page.getByTestId('payment-coming-soon').waitFor({ timeout: 5000 })
log('pressed 시작하기 →', (await page.getByTestId('payment-coming-soon').textContent()).trim(), '✓')
const after = await (await ctx.request.get(`${BASE}/api/me/subscription`)).json()
if (after.active !== false || after.status !== 'unsubscribed') {
  throw new Error(`pressing a button activated a plan: ${JSON.stringify(after)}`)
}
log('subscription after pressing the button:', after.status, '✓')

// There is no route that would grant one.
for (const [method, path] of [['post', '/api/me/subscription'], ['put', '/api/me/subscription'], ['post', '/api/me/plan'], ['post', '/api/plans'], ['post', '/api/set-plan']]) {
  const r = await ctx.request[method](`${BASE}${path}`, { data: { plan_id: 'business' }, failOnStatusCode: false })
  if (r.status() !== 404 && r.status() !== 405) throw new Error(`${method} ${path} answered ${r.status()}`)
}
const still = await (await ctx.request.get(`${BASE}/api/me/subscription`)).json()
if (still.active !== false) throw new Error('self-upgrade succeeded')
log('no route grants a plan; still unsubscribed ✓')

await browser.close()
log('ALL PASS')
