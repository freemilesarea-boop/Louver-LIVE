#!/usr/bin/env node
/**
 * Place the FFmpeg/ffprobe sidecars Tauri bundles (§2, §49).
 *
 * Tauri requires `binaries/<name>-<target-triple>` to exist at build time, so
 * this runs before any build. It tries, in order:
 *
 *   1. A download of the official static build for the target platform.
 *   2. The ffmpeg/ffprobe already on PATH (development machines and CI).
 *
 * Step 2 is what keeps `npm run verify` working on a machine with no network,
 * which is the case in most CI sandboxes. It is explicitly a *development*
 * fallback: a release build must use step 1 so the shipped binary is the
 * licence-cleared build recorded in LICENSES.md.
 *
 * Usage:
 *   node scripts/fetch-ffmpeg.mjs                  # host platform
 *   node scripts/fetch-ffmpeg.mjs --target <triple>
 *   node scripts/fetch-ffmpeg.mjs --require-download   # fail instead of falling back
 *   node scripts/fetch-ffmpeg.mjs --url <u> --probe-url <u> --out <dir>
 *
 * The last three override where the archives come from and where the sidecars
 * are put. They exist so the download path can be exercised against a local
 * server — it is the part that has broken a release twice and the part no
 * offline test could reach — and for anyone mirroring the builds internally.
 */
import { execFileSync, spawnSync } from 'node:child_process'
import {
  existsSync, mkdirSync, copyFileSync, chmodSync, statSync, writeFileSync, readFileSync, readdirSync, rmSync,
} from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { SOURCES, TOOLS, urlsFor, sidecarName, provenanceName } from './sidecar-sources.mjs'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const DEFAULT_OUT = join(ROOT, 'apps/desktop/src-tauri/binaries')

function hostTriple() {
  const { platform, arch } = process
  if (platform === 'win32') return 'x86_64-pc-windows-msvc'
  if (platform === 'darwin') return arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin'
  return arch === 'arm64' ? 'aarch64-unknown-linux-gnu' : 'x86_64-unknown-linux-gnu'
}

function which(name) {
  const cmd = process.platform === 'win32' ? 'where' : 'which'
  const r = spawnSync(cmd, [name], { encoding: 'utf8' })
  if (r.status !== 0) return null
  return r.stdout.split(/\r?\n/)[0].trim() || null
}

/**
 * HTTP statuses that mean "ask again later", not "this is the wrong URL".
 *
 * The v1.0.4 Windows job died on the first of these. gyan.dev answered 503 for
 * the release-essentials zip; the same URL had served the v1.0.3 job ninety
 * minutes earlier and nothing about the request had changed. A 404 is not in
 * here on purpose — a URL that is wrong is wrong on the tenth try too, and the
 * test that proves a missing archive fails fast depends on it.
 */
const TRANSIENT_HTTP = new Set([408, 425, 429, 500, 502, 503, 504])

/** curl exit codes for a network that misbehaved rather than a bad request. */
const TRANSIENT_CURL = new Set([6, 7, 16, 18, 28, 35, 52, 55, 56, 92])

/**
 * How long to wait before each attempt. Four tries, then the release fails.
 *
 * `LOUVER_FETCH_BACKOFF` overrides the waits so `fetch-ffmpeg.test.mjs` can
 * prove the retry without sitting still for eighty seconds. It changes how
 * long this waits and nothing else — not how many sources are tried, not
 * whether the download is required, not what is accepted once it arrives.
 */
const BACKOFF_SECS = (process.env.LOUVER_FETCH_BACKOFF ?? '0,5,20,60')
  .split(',')
  .map(Number)
  .filter((n) => Number.isFinite(n) && n >= 0)

function sleepSync(secs) {
  if (secs > 0) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, secs * 1000)
}

/**
 * Fetch one URL to one file.
 *
 * `-w %{http_code}` rather than reading curl's exit code alone, because 22
 * covers every HTTP status at or above 400 and the decision to retry needs to
 * tell 503 from 404.
 *
 * curl's own `--retry` is deliberately not used. Retrying in two places at
 * once makes the waiting hard to predict and impossible to see: curl retries
 * silently, so a job that sat for a minute looks in the log exactly like one
 * that failed at once. Every attempt this makes is a line in `BACKOFF_SECS`
 * and a line in the log.
 */
function download(url, dest) {
  const args = ['-sS', '-L', '-o', dest, '-w', '%{http_code}', '--fail', '--max-time', '300', url]
  try {
    execFileSync('curl', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] })
    return { ok: true }
  } catch (e) {
    const http = Number(String(e.stdout ?? '').trim()) || 0
    const curl = typeof e.status === 'number' ? e.status : 0
    const why = String(e.stderr || e.message).trim().slice(0, 160)
    return {
      ok: false,
      transient: TRANSIENT_HTTP.has(http) || TRANSIENT_CURL.has(curl),
      detail: http >= 400 ? `HTTP ${http}` : why || `curl exit ${curl}`,
    }
  }
}

/**
 * Download one archive and unpack it into its own directory.
 *
 * Returns `{ dir }`, or `{ transient }` saying whether waiting could help.
 */
function unpack(url, spec, tmp, slot) {
  console.log(`  downloading ${url}`)
  const archive = join(tmp, `archive-${slot}`)
  const got = download(url, archive)
  if (!got.ok) {
    console.log(`  download unavailable: ${got.detail}${got.transient ? ' (retryable)' : ''}`)
    return { transient: got.transient }
  }
  const dir = join(tmp, `x-${slot}`)
  rmSync(dir, { recursive: true, force: true })
  mkdirSync(dir, { recursive: true })
  try {
    if (spec.archive === 'zip') {
      try {
        execFileSync('unzip', ['-q', '-o', archive, '-d', dir])
      } catch {
        // Windows has no unzip, but every Windows 10+ install has bsdtar,
        // which reads zip files.
        execFileSync('tar', ['-xf', archive, '-C', dir])
      }
    } else {
      execFileSync('tar', ['-xf', archive, '-C', dir])
    }
  } catch {
    // A half-written archive from a connection that dropped mid-body unpacks
    // no better than a 503 does, and asking again is the same right answer.
    console.log('  could not unpack the archive (retryable)')
    return { transient: true }
  }
  return { dir }
}

/**
 * Put both sidecars in place from the official build.
 *
 * Returns `{ urls }` they came from, or `{ transient }` saying whether another
 * attempt could succeed.
 *
 * ffmpeg and ffprobe do not always travel together. gyan.dev and
 * johnvansickle ship one archive holding both; evermeet.cx and osxexperts.net
 * publish a separate download per tool, which is what `probeUrl` is for.
 * Reading only `url` on those two meant the archive was fetched, searched for
 * an ffprobe that was never in it, and the whole release failed at
 * "ffprobe was not in the archive" — with the download itself working
 * perfectly. CI never caught it because without --require-download the same
 * miss silently falls through to the runner's own FFmpeg.
 */
function tryDownload(target, spec, tmp, out) {
  const wanted = { ffmpeg: spec.url, ffprobe: spec.probeUrl ?? spec.url }
  const unpacked = new Map()
  const found = {}
  for (const [name, url] of Object.entries(wanted)) {
    if (!unpacked.has(url)) unpacked.set(url, unpack(url, spec, tmp, unpacked.size))
    const got = unpacked.get(url)
    if (!got.dir) return { transient: got.transient }
    const hit = findFile(got.dir, name + spec.exe)
    if (!hit) {
      // The archive is intact and simply does not hold this tool. Downloading
      // it again produces the same archive, so this one does not wait.
      console.log(`  ${name}${spec.exe} was not in ${url}`)
      return { transient: false }
    }
    found[name] = hit
  }
  for (const name of TOOLS) install(found[name], target, name, out)
  return { urls: [...new Set(Object.values(wanted))] }
}

/**
 * The first file called `name` anywhere under `dir`.
 *
 * Walked in Node rather than shelled out to `find`, because on Windows `find`
 * is `C:\\Windows\\System32\\find.exe` — a text search, not a file search. It
 * answers `FIND: Parameter format not correct` and exits 2, which is why the
 * download path had never once worked on Windows: every release build would
 * have failed at the sidecar step before it compiled a line.
 */
function findFile(dir, name) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) {
      const hit = findFile(full, name)
      if (hit) return hit
    } else if (entry.isFile() && entry.name === name) {
      return full
    }
  }
  return null
}

function install(from, target, name, out) {
  mkdirSync(out, { recursive: true })
  const dest = join(out, sidecarName(name, target))
  copyFileSync(from, dest)
  if (process.platform !== 'win32') chmodSync(dest, 0o755)
  console.log(`  ${name} -> ${dest} (${(statSync(dest).size / 1e6).toFixed(1)} MB)`)
}

function main() {
  const args = process.argv.slice(2)
  const flag = (name) => (args.includes(name) ? args[args.indexOf(name) + 1] : null)
  const target = flag('--target') ?? hostTriple()
  const requireDownload = args.includes('--require-download')
  const base = SOURCES[target]
  if (!base) {
    console.error(`unknown target triple: ${target}`)
    process.exit(1)
  }
  const OUT = resolve(flag('--out') ?? DEFAULT_OUT)
  const urls = urlsFor(target, flag('--url'))
  const spec = { ...base, url: urls.ffmpeg, probeUrl: flag('--probe-url') ?? urls.ffprobe }

  const already = TOOLS.every((n) => existsSync(join(OUT, sidecarName(n, target))))
  // What the sidecars presently there actually are. A release must ship the
  // downloaded, licence-cleared static build; the development fallback is the
  // machine's own FFmpeg, dynamically linked against libraries a user's
  // computer does not have, so shipping one produces an app that cannot
  // broadcast at all.
  const sourceFile = join(OUT, provenanceName(target))
  const provenance = existsSync(sourceFile) ? readFileSync(sourceFile, 'utf8') : ''
  const isFallback = !provenance || provenance.includes('DEVELOPMENT ONLY')

  if (already && requireDownload && isFallback) {
    // Not an error yet: the download below may well succeed and replace them.
    // An error here would fail a release build that was about to be correct.
    console.log(`sidecars for ${target} are the development fallback; re-fetching for a release build`)
  } else if (already && !args.includes('--force')) {
    console.log(`sidecars for ${target} already present; use --force to replace them`)
    return
  }

  console.log(`fetching FFmpeg sidecars for ${target}`)
  mkdirSync(OUT, { recursive: true })
  const tmp = join(ROOT, 'node_modules/.cache/ffmpeg-fetch')
  mkdirSync(tmp, { recursive: true })

  // Each source is tried up to four times, and only while the reason to
  // think again is a reason that waiting could fix. A 503 from the provider
  // failed the whole v1.0.4 Windows release in 1.2 seconds — before Rust,
  // before Tauri, before anything this repository controls — and the same URL
  // had served the release ninety minutes before. A wrong URL or an archive
  // that does not hold ffprobe still fails on the first try, because no amount
  // of waiting changes either.
  for (const url of [spec.url, spec.fallbackUrl].filter(Boolean)) {
    for (const [attempt, wait] of BACKOFF_SECS.entries()) {
      if (wait) console.log(`  retrying in ${wait}s (attempt ${attempt + 1} of ${BACKOFF_SECS.length})`)
      sleepSync(wait)
      const got = tryDownload(target, { ...spec, url }, tmp, OUT)
      if (got.urls) {
        writeFileSync(join(OUT, provenanceName(target)), `${got.urls.join('\n')}\n${base.license}\n`)
        console.log('done (downloaded)')
        return
      }
      if (!got.transient) break
    }
  }

  if (requireDownload) {
    console.error(
      '\nFAILED: no static build could be downloaded, and --require-download was set.\n' +
        'Release builds must ship the downloaded, licence-cleared binaries. See LICENSES.md.' +
        (already && isFallback
          ? '\nThe sidecars already in place are the development fallback and were NOT shipped.'
          : ''),
    )
    process.exit(1)
  }

  // Development fallback.
  if (target !== hostTriple()) {
    console.error(`cannot fall back to the system FFmpeg for a foreign target (${target})`)
    process.exit(1)
  }
  const sys = { ffmpeg: which('ffmpeg'), ffprobe: which('ffprobe') }
  if (!sys.ffmpeg || !sys.ffprobe) {
    console.error(
      '\nFAILED: could not download a static build, and no ffmpeg/ffprobe is on PATH.\n' +
        'Install FFmpeg (brew install ffmpeg / winget install Gyan.FFmpeg / apt install ffmpeg)\n' +
        'or run this script on a machine with network access.',
    )
    process.exit(1)
  }
  console.log('  download unavailable; using the system FFmpeg (development only)')
  for (const name of TOOLS) install(sys[name], target, name, OUT)
  writeFileSync(
    join(OUT, provenanceName(target)),
    `system PATH fallback: ${sys.ffmpeg}\nDEVELOPMENT ONLY - not licence-cleared for redistribution\n`,
  )
  console.log('done (system fallback)')
}

main()
