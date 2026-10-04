/**
 * Proves the SEO checker actually checks.
 *
 * A validator that passes everything is worse than no validator: it is a green
 * step that means nothing. So this builds a small `dist` that the checker
 * accepts, then breaks one thing at a time and insists it is caught — including
 * the mistakes that would be most expensive to make, like a canonical on the
 * wrong origin or a page whose content is not in the HTML at all.
 *
 * The real `dist` is checked by `npm run seo:check` in `scripts/verify.mjs`,
 * against the files that are about to be deployed.
 */
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { execFile } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { promisify } from 'node:util'
import { beforeAll, afterAll, describe, expect, it } from 'vitest'
import { GIB, seedPlans } from './seo-plan-source.mjs'

const run = promisify(execFile)
const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const ORIGIN = 'https://247streams.kr'

const ROUTES = [
  { path: '/', file: 'index.html' },
  { path: '/youtube-24-live/', file: 'youtube-24-live/index.html' },
  { path: '/playlist-live/', file: 'playlist-live/index.html' },
  { path: '/youtube-live-streaming/', file: 'youtube-live-streaming/index.html' },
  { path: '/pricing/', file: 'pricing/index.html' },
  { path: '/terms/', file: 'terms/index.html' },
  { path: '/privacy/', file: 'privacy/index.html' },
]

/** Filler with none of the words the keyword rule watches. */
const FILLER =
  '여기에는 페이지를 설명하는 문장이 들어갑니다. '.repeat(10) +
  '필요한 만큼만 쓰고, 읽는 사람에게 도움이 되는 내용을 적습니다. '.repeat(10)

function head(route, over = {}) {
  const title = over.title ?? `${route.path} 제목`
  const description =
    over.description ??
    `${route.path} 페이지가 무엇을 설명하는지 한 문장으로 적어 둔 설명문이며, 길이 조건을 넘기기 위해 충분히 길게 씁니다.`
  const canonical = over.canonical ?? `${ORIGIN}${route.path}`
  const ld = over.ld ?? {
    '@context': 'https://schema.org',
    '@graph': [
      { '@type': 'Organization', name: '247streams' },
      { '@type': 'WebSite', name: '247streams' },
    ],
  }
  return [
    `<title>${title}</title>`,
    `<meta name="description" content="${description}" />`,
    `<meta name="robots" content="${over.robots ?? 'index,follow,max-image-preview:large'}" />`,
    `<link rel="canonical" href="${canonical}" />`,
    `<meta property="og:site_name" content="247streams" />`,
    `<meta property="og:locale" content="ko_KR" />`,
    `<meta property="og:title" content="${title}" />`,
    `<meta property="og:description" content="${description}" />`,
    `<meta property="og:url" content="${over.ogUrl ?? `${ORIGIN}${route.path}`}" />`,
    `<meta name="twitter:card" content="summary" />`,
    `<script type="application/ld+json">${JSON.stringify(ld)}</script>`,
  ].join('\n    ')
}

async function planBlock() {
  const plans = await seedPlans(ROOT)
  return Object.entries(plans)
    .filter(([, p]) => p.active)
    .map(
      ([, p]) =>
        `<section><h3>${p.label}</h3><p>₩${p.monthly_price_krw.toLocaleString('en-US')}</p>` +
        `<p>${p.limits.max_storage_bytes / GIB}GB</p><p>${p.limits.max_upload_bytes / GIB}GB</p>` +
        `<p>${p.limits.max_concurrent_streams}개</p></section>`,
    )
    .join('')
}

/** Links to every route, so nothing is orphaned. */
const NAV = ROUTES.map((r) => `<a href="${r.path}">${r.path}</a>`).join('')

async function page(route, over = {}) {
  const body =
    (over.body ?? `<h1>${route.path} 제목</h1>`) +
    (route.path === '/pricing/' && !over.noPlans ? await planBlock() : '') +
    `<p>${FILLER}</p>`
  return [
    '<!doctype html>',
    '<html lang="ko"><head>',
    `    ${head(route, over)}`,
    '<link rel="stylesheet" crossorigin href="/assets/index.css">',
    '<script type="module" crossorigin src="/assets/index.js"></script>',
    '</head>',
    `<body><header>${NAV}</header><main>${body}</main><footer>${NAV}</footer>`,
    `<div id="root">${over.root ?? body}</div>`,
    '</body></html>',
  ].join('\n')
}

async function build(dir, tweaks = {}) {
  await mkdir(join(dir, 'assets'), { recursive: true })
  await writeFile(join(dir, 'assets/index.js'), '// bundle')
  await writeFile(join(dir, 'assets/index.css'), '/* styles */')
  for (const route of ROUTES) {
    if (tweaks.omit === route.path) continue
    const out = join(dir, route.file)
    await mkdir(dirname(out), { recursive: true })
    await writeFile(out, await page(route, tweaks[route.path] ?? {}))
  }
  await writeFile(
    join(dir, 'robots.txt'),
    tweaks.robots ??
      ['User-agent: *', 'Allow: /', 'Disallow: /admin', 'Disallow: /billing/complete', 'Disallow: /api/', '', `Sitemap: ${ORIGIN}/sitemap.xml`, ''].join('\n'),
  )
  await writeFile(
    join(dir, 'sitemap.xml'),
    tweaks.sitemap ??
      [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">',
        ...ROUTES.map((r) => `  <url><loc>${ORIGIN}${r.path}</loc></url>`),
        '</urlset>',
      ].join('\n'),
  )
}

/** Runs the checker over a dist built with `tweaks`, and returns its output. */
async function check(tweaks = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'seo-check-'))
  try {
    await build(dir, tweaks)
    try {
      const { stdout } = await run('node', [join(ROOT, 'scripts/seo-check.mjs')], {
        env: { ...process.env, SEO_DIST: dir },
      })
      return { ok: true, out: stdout }
    } catch (e) {
      return { ok: false, out: `${e.stdout ?? ''}${e.stderr ?? ''}` }
    }
  } finally {
    await rm(dir, { recursive: true, force: true })
  }
}

describe('seo-check', () => {
  it('passes a dist that has everything it asks for', async () => {
    const { ok, out } = await check()
    expect(out).not.toContain('✗')
    expect(ok).toBe(true)
  })

  it('catches a page that was never written', async () => {
    const { ok, out } = await check({ omit: '/pricing/' })
    expect(ok).toBe(false)
    expect(out).toContain('pricing/index.html was not written')
  })

  it('catches two pages sharing one title', async () => {
    const { ok, out } = await check({
      '/playlist-live/': { title: '/pricing/ 제목' },
    })
    expect(ok).toBe(false)
    expect(out).toMatch(/<title> is the same as/)
  })

  it('catches a canonical that points somewhere else', async () => {
    const { ok, out } = await check({
      '/pricing/': { canonical: 'http://247streams.kr/pricing' },
    })
    expect(ok).toBe(false)
    expect(out).toContain('canonical is http://247streams.kr/pricing')
  })

  it('catches a page whose content is not in the HTML', async () => {
    const { ok, out } = await check({ '/pricing/': { root: '' } })
    expect(ok).toBe(false)
    expect(out).toMatch(/#root holds \d+ characters/)
  })

  it('catches a second h1', async () => {
    const { ok, out } = await check({
      '/terms/': { body: '<h1>하나</h1><h1>둘</h1>' },
    })
    expect(ok).toBe(false)
    expect(out).toContain('2 <h1> elements')
  })

  it('catches a claim nobody can verify', async () => {
    const { ok, out } = await check({
      '/youtube-24-live/': {
        body: '<h1>제목</h1><p>절대 끊기지 않으며 100% 안정적으로 동작합니다.</p>',
      },
    })
    expect(ok).toBe(false)
    expect(out).toContain('unverifiable claim')
  })

  it('catches invented structured data', async () => {
    const { ok, out } = await check({
      '/': {
        ld: {
          '@context': 'https://schema.org',
          '@graph': [
            { '@type': 'Organization', name: '247streams' },
            { '@type': 'WebSite', name: '247streams' },
            { '@type': 'Product', aggregateRating: { ratingValue: '4.9', reviewCount: 120 } },
          ],
        },
      },
    })
    expect(ok).toBe(false)
    expect(out).toContain('JSON-LD contains aggregateRating')
  })

  it('catches a noindex page that is still in the sitemap', async () => {
    const { ok, out } = await check({ '/pricing/': { robots: 'noindex,follow' } })
    expect(ok).toBe(false)
    expect(out).toMatch(/robots is "noindex,follow"/)
  })

  it('catches a sitemap that offers a private path', async () => {
    const { ok, out } = await check({
      sitemap: [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">',
        ...ROUTES.map((r) => `  <url><loc>${ORIGIN}${r.path}</loc></url>`),
        `  <url><loc>${ORIGIN}/admin</loc></url>`,
        '</urlset>',
      ].join('\n'),
    })
    expect(ok).toBe(false)
    expect(out).toContain('/admin, which has no page')
  })

  it('catches a robots.txt that forgets the sitemap', async () => {
    const { ok, out } = await check({
      robots: ['User-agent: *', 'Allow: /', 'Disallow: /admin', 'Disallow: /billing/complete', 'Disallow: /api/', ''].join('\n'),
    })
    expect(ok).toBe(false)
    expect(out).toContain('does not declare the sitemap')
  })

  it('catches a price that is not the price the server charges', async () => {
    const { ok, out } = await check({
      '/pricing/': { noPlans: true, body: '<h1>요금제</h1><p>Basic Pro Business ₩9,900 / 월</p>' },
    })
    expect(ok).toBe(false)
    expect(out).toMatch(/does not show .*'s price/)
  })
})
