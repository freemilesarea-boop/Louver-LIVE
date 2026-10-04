#!/usr/bin/env node
/**
 * Writes the public pages as static HTML, after `vite build`.
 *
 * Why this exists: the front end is a single-page app, so every URL is served
 * one `index.html` whose `#root` is empty until a script runs. A crawler that
 * fetches HTML and reads it — which is still how a page's title, description
 * and canonical are collected — therefore sees one title for every URL and no
 * content at all. Google's renderer would eventually run the script, but the
 * page it indexes would still be the one the server sent, and that page is a
 * sign-in form for every public URL.
 *
 * What it does, without changing the server or the framework:
 *
 *   1. `vite build` produces `apps/web/dist` as it always has, including the
 *      hashed `<script>` and `<link>` tags in `index.html`. Untouched.
 *   2. `vite build --config vite.seo.config.ts` compiles the marketing pages
 *      for Node, into `apps/web/.seo-ssr/` (a build artefact, never deployed).
 *   3. This script renders each public page with those components, pastes the
 *      markup into `#root` and that page's tags into `<head>`, and writes
 *      `dist/<route>/index.html` — which `ServeDir` already serves for
 *      `/<route>/`, so the server needs no change.
 *   4. It writes `robots.txt` and `sitemap.xml` from the same route table.
 *
 * The browser bundle is identical to before. When a visitor opens one of these
 * URLs with JavaScript, `App.tsx` renders the same component from the same
 * content module, so the static HTML and the live page say the same thing.
 */
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

// `fileURLToPath` rather than `import.meta.dirname`, which Node only gained
// in 20.11 — the build image is `node:20-bookworm`, whatever patch that is.
const ROOT = resolve(fileURLToPath(new URL('.', import.meta.url)), '..')
const DIST = join(ROOT, 'apps/web/dist')
const BUNDLE = join(ROOT, 'apps/web/.seo-ssr/prerender-entry.mjs')

/** Where the shell's replaceable head lives, as written in `index.html`. */
const HEAD_MARKERS = /<!--seo:head-start-->[\s\S]*?<!--seo:head-end-->/
const EMPTY_ROOT = /<div id="root">\s*<\/div>/

function fail(message) {
  console.error(`seo-prerender: ${message}`)
  process.exit(1)
}

/** `/` → `dist/index.html`, `/pricing/` → `dist/pricing/index.html`. */
function fileFor(path) {
  if (path === '/') return join(DIST, 'index.html')
  const clean = path.replace(/^\/+|\/+$/g, '')
  if (clean === '' || clean.includes('..')) fail(`refusing to write ${path}`)
  return join(DIST, clean, 'index.html')
}

/**
 * The built shell, with vite's hashed asset tags in it.
 *
 * `dist/index.html` is both the template and one of the outputs — it is the
 * file `/` is served from — so the first run fills it in and a second run would
 * find no markers left. The untouched copy is therefore stashed next to the
 * prerender bundle, which makes running this script twice do the same thing
 * twice. A stale stash is caught below, by checking that the assets it names
 * are still in `dist`.
 */
const STASH = join(ROOT, 'apps/web/.seo-shell.html')

async function readShell() {
  // `dist/index.html` first: a fresh `vite build` has just written it, with
  // this build's hashed asset names in it. The stash is the fallback for a
  // re-run, when that file has already been filled in.
  for (const path of [join(DIST, 'index.html'), STASH]) {
    let text
    try {
      text = await readFile(path, 'utf8')
    } catch {
      continue
    }
    if (HEAD_MARKERS.test(text) && EMPTY_ROOT.test(text)) return text
  }
  return null
}

const shell = await readShell()
// A silent mismatch here would publish pages with no title and an empty body,
// which is worse than failing the build.
if (!shell) {
  fail(
    'no usable shell: run `vite build --config vite.web.config.ts`, and keep the\n' +
      '  <!--seo:head-start--> markers and the empty <div id="root"> in apps/web/index.html',
  )
}

// The stash is only valid for the build that is in `dist` now.
for (const asset of shell.match(/\/assets\/[A-Za-z0-9._-]+/g) ?? []) {
  try {
    await readFile(join(DIST, asset))
  } catch {
    fail(`the shell references ${asset}, which is not in dist — rebuild the web bundle`)
  }
}
await mkdir(dirname(STASH), { recursive: true })
await writeFile(STASH, shell, 'utf8')

let mod
try {
  mod = await import(pathToFileURL(BUNDLE).href)
} catch (e) {
  fail(`${BUNDLE} is missing or failed to load — run \`vite build --config vite.seo.config.ts\` first\n${e}`)
}

const pages = mod.prerender()
if (pages.length === 0) fail('the route table is empty')

for (const page of pages) {
  const html = shell
    .replace(HEAD_MARKERS, page.head)
    .replace(EMPTY_ROOT, `<div id="root">${page.body}</div>`)
  const out = fileFor(page.path)
  await mkdir(dirname(out), { recursive: true })
  await writeFile(out, html, 'utf8')
  console.log(`  ${page.path.padEnd(26)} → ${out.slice(DIST.length + 1)}`)
}

await writeFile(join(DIST, 'robots.txt'), mod.robotsTxt(), 'utf8')
await writeFile(join(DIST, 'sitemap.xml'), mod.sitemapXml(), 'utf8')
console.log(`  robots.txt and sitemap.xml  → ${mod.SITE_ORIGIN}`)
console.log(`seo-prerender: ${pages.length} page(s) written`)
