#!/usr/bin/env node
/**
 * Checks the HTML that will actually be deployed.
 *
 * Not the components, not the content module — the files in `apps/web/dist`,
 * after `npm run build:cloud`. Everything an SEO review would open the page
 * source to look at is asserted here, so a regression fails a build instead of
 * being noticed in Search Console six weeks later:
 *
 *   - every public URL has its own file, title, description and canonical;
 *   - titles and descriptions are unique, and the canonical is absolute, on the
 *     production origin, in the one spelling the server serves;
 *   - the body is really in the HTML (a crawler that runs no script sees it),
 *     with exactly one `<h1>`;
 *   - every internal link points at a URL that exists;
 *   - the structured data parses, describes what the page shows, and invents no
 *     rating or review;
 *   - the prices in the HTML equal `SEED_PLANS` in the server's source;
 *   - `robots.txt` and `sitemap.xml` agree with the route table and with each
 *     other, and neither offers a private path to a crawler;
 *   - no unverifiable marketing claim and no keyword stuffing.
 *
 * Usage: `npm run seo:check` (after a build). Exits non-zero on the first
 * failing group, having printed every failure it found.
 */
import { readFile, readdir, stat } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { GIB, PLAN_SOURCE, seedPlans } from './seo-plan-source.mjs'

const ROOT = resolve(fileURLToPath(new URL('.', import.meta.url)), '..')
const DIST = process.env.SEO_DIST ?? join(ROOT, 'apps/web/dist')
const ORIGIN = 'https://247streams.kr'

/** The URLs that must exist, and whether a crawler may index them. */
const EXPECTED = [
  { path: '/', file: 'index.html', indexable: true },
  { path: '/youtube-24-live/', file: 'youtube-24-live/index.html', indexable: true },
  { path: '/playlist-live/', file: 'playlist-live/index.html', indexable: true },
  { path: '/youtube-live-streaming/', file: 'youtube-live-streaming/index.html', indexable: true },
  { path: '/pricing/', file: 'pricing/index.html', indexable: true },
  { path: '/terms/', file: 'terms/index.html', indexable: true },
  { path: '/privacy/', file: 'privacy/index.html', indexable: true },
]

/** Paths a crawler must be told to stay away from. */
const PRIVATE = ['/admin', '/billing/complete', '/api/']

/**
 * Claims a page may not make.
 *
 * Each one is a promise nobody can keep or check: an uptime figure, an absolute
 * guarantee, a result on somebody else's channel. The patterns match the claim
 * and not the word — "보장하지는 않습니다" is the opposite of a guarantee and is
 * exactly the kind of sentence that should be allowed to stay.
 */
const FORBIDDEN_CLAIMS = [
  /100\s*%/,
  /무조건/,
  /절대\s*(안\s*)?끊기/,
  /끊기지\s*않습니다/,
  /보장(합니다|됩니다|해\s*드립니다|드립니다)/,
  /완벽(한|하게|히|합니다)/,
  /업계\s*(최고|1위|일위)/,
  /최고의/,
  /수익(을|이)\s*(보장|약속)/,
  /조회수(가|를)\s*(오르|올라|증가|상승)/,
  /구독자(가|를)\s*(늘어|늘려|증가)/,
  /무중단/,
  /장애\s*없/,
]

/**
 * Words a page is allowed to use, but not to repeat into the ground.
 *
 * Measured by the share of the page's characters a keyword accounts for, not by
 * word count: Korean does not space its words the way English does, so counting
 * words makes an ordinary page look stuffed. The ceiling is deliberately high —
 * a page about playlists will say "플레이리스트" often, and should. What it
 * catches is the thing it is meant to catch: copy written for a crawler, where
 * one phrase is a tenth of the text.
 *
 * Below `SHORT_PAGE` a share means little, because one mention on a short page
 * is already a few percent. There the rule is an absolute count instead.
 */
const KEYWORDS = ['24시간', '라이브', '유튜브', 'YouTube', '송출', '플레이리스트', '스트리밍']
const MAX_KEYWORD_SHARE = 0.06
const SHORT_PAGE = 600
const MAX_SHORT_PAGE_REPEATS = 5

const failures = []
function check(ok, message) {
  if (!ok) failures.push(message)
}

async function read(file) {
  return readFile(join(DIST, file), 'utf8')
}

async function exists(file) {
  try {
    await stat(join(DIST, file))
    return true
  } catch {
    return false
  }
}

/** The text a reader sees: tags, scripts and styles removed. */
function visibleText(html) {
  const body = html.slice(html.indexOf('<body'))
  return body
    .replace(/<script[\s\S]*?<\/script>/g, ' ')
    .replace(/<style[\s\S]*?<\/style>/g, ' ')
    .replace(/<[^>]+>/g, ' ')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&')
    .replace(/\s+/g, ' ')
    .trim()
}

/**
 * The page's own text, without the shared header and footer.
 *
 * Keyword share is measured on this and not on the whole page: the navigation
 * and the footer repeat the same link labels everywhere, which on a short page
 * is most of the text and none of the writing.
 */
function mainText(html) {
  const start = html.indexOf('<main')
  const end = html.indexOf('</main>')
  if (start < 0 || end < 0) return visibleText(html)
  return visibleText(`<body>${html.slice(start, end)}`)
}

function attr(html, pattern) {
  return html.match(pattern)?.[1] ?? null
}

function metaContent(html, name) {
  const byName = new RegExp(`<meta\\s+name="${name}"\\s+content="([^"]*)"`)
  const byProperty = new RegExp(`<meta\\s+property="${name}"\\s+content="([^"]*)"`)
  return attr(html, byName) ?? attr(html, byProperty)
}

/* ------------------------------------------------------------------ pages */

const titles = new Map()
const descriptions = new Map()
const linked = new Set()
const pages = []

for (const page of EXPECTED) {
  if (!(await exists(page.file))) {
    failures.push(`${page.path}: ${page.file} was not written — run \`npm run build:cloud\``)
    continue
  }
  const html = await read(page.file)
  pages.push({ ...page, html })

  const title = attr(html, /<title>([^<]*)<\/title>/)
  check(!!title && title.trim().length > 0, `${page.path}: no <title>`)
  if (title) {
    check(title.length <= 75, `${page.path}: <title> is ${title.length} characters (max 75)`)
    const seen = titles.get(title)
    check(!seen, `${page.path}: <title> is the same as ${seen}'s`)
    titles.set(title, page.path)
  }

  const description = metaContent(html, 'description')
  check(!!description, `${page.path}: no meta description`)
  if (description) {
    check(
      description.length >= 50 && description.length <= 170,
      `${page.path}: meta description is ${description.length} characters (want 50–170)`,
    )
    const seen = descriptions.get(description)
    check(!seen, `${page.path}: meta description is the same as ${seen}'s`)
    descriptions.set(description, page.path)
  }

  const canonical = attr(html, /<link rel="canonical" href="([^"]*)"/)
  check(canonical === `${ORIGIN}${page.path}`, `${page.path}: canonical is ${canonical}`)

  const robots = metaContent(html, 'robots')
  check(!!robots, `${page.path}: no robots meta`)
  if (robots) {
    check(
      page.indexable ? robots.startsWith('index') : robots.includes('noindex'),
      `${page.path}: robots is "${robots}" but indexable=${page.indexable}`,
    )
  }

  for (const [tag, want] of [
    ['og:title', title],
    ['og:description', description],
    ['og:url', `${ORIGIN}${page.path}`],
  ]) {
    check(metaContent(html, tag) === want, `${page.path}: ${tag} does not match the page`)
  }
  check(!!metaContent(html, 'og:site_name'), `${page.path}: no og:site_name`)
  check(metaContent(html, 'og:locale') === 'ko_KR', `${page.path}: no Korean og:locale`)
  check(!!metaContent(html, 'twitter:card'), `${page.path}: no twitter:card`)

  // The point of the whole exercise: the content is in the HTML the server
  // sends, not only in whatever a script would build afterwards.
  const root = html.match(/<div id="root">([\s\S]*)<\/div>\s*<\/body>/)?.[1] ?? ''
  check(root.trim().length > 500, `${page.path}: #root holds ${root.trim().length} characters of HTML`)
  const h1s = root.match(/<h1[\s>]/g) ?? []
  check(h1s.length === 1, `${page.path}: ${h1s.length} <h1> elements (want exactly 1)`)

  // The app still has to boot: the prerender must not have dropped vite's tags.
  check(/<script type="module"[^>]+src="\/assets\//.test(html), `${page.path}: no module script tag`)
  check(/<link rel="stylesheet"[^>]+href="\/assets\//.test(html), `${page.path}: no stylesheet tag`)

  const text = visibleText(html)
  for (const claim of FORBIDDEN_CLAIMS) {
    const hit = text.match(claim)
    check(!hit, `${page.path}: unverifiable claim "${hit?.[0]}" (${claim})`)
  }

  const body = mainText(html)
  for (const keyword of KEYWORDS) {
    const count = body.split(keyword).length - 1
    const share = (count * keyword.length) / Math.max(body.length, 1)
    if (body.length < SHORT_PAGE) {
      check(
        count <= MAX_SHORT_PAGE_REPEATS,
        `${page.path}: "${keyword}" ${count}x in ${body.length} characters of body text (max ${MAX_SHORT_PAGE_REPEATS})`,
      )
    } else {
      check(
        share <= MAX_KEYWORD_SHARE,
        `${page.path}: "${keyword}" is ${count}x and ${(share * 100).toFixed(1)}% of the text (max ${(MAX_KEYWORD_SHARE * 100).toFixed(1)}%)`,
      )
    }
    if (process.env.SEO_DENSITY) {
      console.log(`  ${page.path} ${keyword}: ${count}x ${(share * 100).toFixed(2)}% of ${body.length} chars`)
    }
  }

  for (const [, href] of html.matchAll(/href="(\/[^"#]*)"/g)) {
    if (href.startsWith('/assets/') || href === '/favicon.svg') continue
    linked.add(href)
  }

  // Structured data: it parses, it says what the page says, and it invents
  // nothing. A FAQ answer in the markup that is not on the page is the exact
  // thing Google penalises, so each question is looked for in the text.
  const ld = html.match(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/)
  check(!!ld, `${page.path}: no JSON-LD`)
  if (ld) {
    let parsed
    try {
      parsed = JSON.parse(ld[1])
    } catch (e) {
      failures.push(`${page.path}: JSON-LD does not parse: ${e.message}`)
    }
    if (parsed) {
      const graph = parsed['@graph'] ?? []
      const types = graph.map((n) => n['@type'])
      check(types.includes('Organization'), `${page.path}: no Organization in JSON-LD`)
      check(types.includes('WebSite'), `${page.path}: no WebSite in JSON-LD`)
      const serialised = JSON.stringify(parsed)
      for (const invented of ['aggregateRating', 'AggregateRating', 'review', 'Review', 'ratingValue']) {
        check(!serialised.includes(invented), `${page.path}: JSON-LD contains ${invented}`)
      }
      const faq = graph.find((n) => n['@type'] === 'FAQPage')
      for (const q of faq?.mainEntity ?? []) {
        check(text.includes(q.name), `${page.path}: FAQ question is in the schema but not on the page: ${q.name}`)
        check(
          text.includes(q.acceptedAnswer.text),
          `${page.path}: FAQ answer is in the schema but not on the page: ${q.name}`,
        )
      }
    }
  }
}

/* ------------------------------------------------------------------ links */

for (const href of [...linked].sort()) {
  const target = EXPECTED.find((p) => p.path === href)
  check(!!target, `internal link to ${href}, which no page answers`)
}
// Every page must be reachable from another page, or a crawler that finds one
// of them finds nothing else.
for (const page of EXPECTED) {
  if (page.path === '/') continue
  check(linked.has(page.path), `${page.path} is in the sitemap but nothing links to it`)
}

/* -------------------------------------------------------------- the files */

if (await exists('robots.txt')) {
  const robots = await read('robots.txt')
  check(robots.includes(`Sitemap: ${ORIGIN}/sitemap.xml`), 'robots.txt does not declare the sitemap')
  check(/^User-agent: \*/m.test(robots), 'robots.txt has no wildcard user-agent group')
  check(/^Allow: \/$/m.test(robots), 'robots.txt does not allow the public pages')
  for (const path of PRIVATE) {
    check(robots.includes(`Disallow: ${path}`), `robots.txt does not disallow ${path}`)
  }
  for (const page of EXPECTED) {
    check(
      !new RegExp(`^Disallow: ${page.path}$`, 'm').test(robots),
      `robots.txt disallows ${page.path}, which is a public page`,
    )
  }
} else {
  failures.push('robots.txt was not written')
}

if (await exists('sitemap.xml')) {
  const sitemap = await read('sitemap.xml')
  const locs = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1])
  check(locs.length > 0, 'sitemap.xml lists no URLs')
  for (const loc of locs) {
    check(loc.startsWith(`${ORIGIN}/`), `sitemap.xml lists ${loc}, which is not on ${ORIGIN}`)
    const path = loc.slice(ORIGIN.length)
    const page = EXPECTED.find((p) => p.path === path)
    check(!!page, `sitemap.xml lists ${path}, which has no page`)
    check(page?.indexable !== false, `sitemap.xml lists ${path}, which is noindex`)
    for (const priv of PRIVATE) {
      check(!path.startsWith(priv), `sitemap.xml lists the private path ${path}`)
    }
  }
  for (const page of EXPECTED.filter((p) => p.indexable)) {
    check(locs.includes(`${ORIGIN}${page.path}`), `sitemap.xml is missing ${page.path}`)
  }
  check(new Set(locs).size === locs.length, 'sitemap.xml lists a URL twice')
} else {
  failures.push('sitemap.xml was not written')
}

/* ------------------------------------------------------------- the prices */

const plans = await seedPlans(ROOT)
const pricing = pages.find((p) => p.path === '/pricing/')
if (pricing) {
  const text = visibleText(pricing.html)
  for (const [id, plan] of Object.entries(plans)) {
    if (!plan.active) continue
    const won = `₩${plan.monthly_price_krw.toLocaleString('en-US')}`
    check(text.includes(plan.label), `/pricing/ does not name the ${id} plan ("${plan.label}")`)
    check(text.includes(won), `/pricing/ does not show ${id}'s price ${won} from ${PLAN_SOURCE}`)
    const storage = `${plan.limits.max_storage_bytes / GIB}GB`
    const upload = `${plan.limits.max_upload_bytes / GIB}GB`
    check(text.includes(storage), `/pricing/ does not show ${id}'s storage limit ${storage}`)
    check(text.includes(upload), `/pricing/ does not show ${id}'s upload limit ${upload}`)
    check(
      text.includes(`${plan.limits.max_concurrent_streams}개`),
      `/pricing/ does not show ${id}'s concurrency ${plan.limits.max_concurrent_streams}`,
    )
  }
}

/* --------------------------------------------------------------- the rest */

// Nothing may be published under a route directory that the route table does
// not know about: a stale directory from a renamed page would stay indexed.
const entries = await readdir(DIST, { withFileTypes: true })
for (const entry of entries) {
  if (!entry.isDirectory() || entry.name === 'assets') continue
  check(
    EXPECTED.some((p) => p.file === `${entry.name}/index.html`),
    `dist/${entry.name}/ is published but is not in the route table`,
  )
}

/* ------------------------------------------------------------------ done */

if (failures.length > 0) {
  console.error(`seo-check: ${failures.length} problem(s)\n`)
  for (const f of failures) console.error(`  ✗ ${f}`)
  process.exit(1)
}
console.log(`seo-check: ${EXPECTED.length} page(s), robots.txt and sitemap.xml all pass`)
