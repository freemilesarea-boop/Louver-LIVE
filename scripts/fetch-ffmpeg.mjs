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
 */
import { execFileSync, spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, copyFileSync, chmodSync, statSync, writeFileSync, readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const OUT = join(ROOT, 'apps/desktop/src-tauri/binaries')

/** Tauri target triples, and where an official static build comes from. */
const SOURCES = {
  'x86_64-pc-windows-msvc': {
    url: 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip',
    archive: 'zip',
    exe: '.exe',
    license: 'GPL v3 (gyan.dev release-essentials)',
  },
  'x86_64-apple-darwin': {
    url: 'https://evermeet.cx/ffmpeg/getrelease/zip',
    probeUrl: 'https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip',
    archive: 'zip',
    exe: '',
    license: 'GPL v3 (evermeet.cx)',
  },
  'aarch64-apple-darwin': {
    url: 'https://www.osxexperts.net/ffmpeg711arm.zip',
    probeUrl: 'https://www.osxexperts.net/ffprobe711arm.zip',
    archive: 'zip',
    exe: '',
    license: 'GPL v3 (osxexperts.net)',
  },
  'x86_64-unknown-linux-gnu': {
    url: 'https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz',
    archive: 'tar.xz',
    exe: '',
    license: 'GPL v3 (johnvansickle.com static)',
  },
  'aarch64-unknown-linux-gnu': {
    url: 'https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz',
    archive: 'tar.xz',
    exe: '',
    license: 'GPL v3 (johnvansickle.com static)',
  },
}

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

function tryDownload(target, spec, tmp) {
  console.log(`  downloading ${spec.url}`)
  try {
    execFileSync('curl', ['-fsSL', '--max-time', '300', '-o', join(tmp, 'ffmpeg-archive'), spec.url], {
      stdio: ['ignore', 'ignore', 'pipe'],
    })
  } catch (e) {
    console.log(`  download unavailable: ${String(e.stderr || e.message).trim().slice(0, 160)}`)
    return false
  }
  mkdirSync(join(tmp, 'x'), { recursive: true })
  try {
    if (spec.archive === 'zip') {
      execFileSync('unzip', ['-q', '-o', join(tmp, 'ffmpeg-archive'), '-d', join(tmp, 'x')])
    } else {
      execFileSync('tar', ['-xf', join(tmp, 'ffmpeg-archive'), '-C', join(tmp, 'x')])
    }
  } catch {
    console.log('  could not unpack the archive')
    return false
  }
  const found = {}
  for (const name of ['ffmpeg', 'ffprobe']) {
    const hit = execFileSync('find', [join(tmp, 'x'), '-type', 'f', '-name', name + spec.exe], { encoding: 'utf8' })
      .split('\n')
      .filter(Boolean)[0]
    if (!hit) return false
    found[name] = hit
  }
  for (const name of ['ffmpeg', 'ffprobe']) {
    install(found[name], target, name, spec.exe)
  }
  return true
}

function install(from, target, name, exe) {
  mkdirSync(OUT, { recursive: true })
  const dest = join(OUT, `${name}-${target}${exe}`)
  copyFileSync(from, dest)
  if (process.platform !== 'win32') chmodSync(dest, 0o755)
  console.log(`  ${name} -> ${dest} (${(statSync(dest).size / 1e6).toFixed(1)} MB)`)
}

function main() {
  const args = process.argv.slice(2)
  const target = args.includes('--target') ? args[args.indexOf('--target') + 1] : hostTriple()
  const requireDownload = args.includes('--require-download')
  const spec = SOURCES[target]
  if (!spec) {
    console.error(`unknown target triple: ${target}`)
    process.exit(1)
  }

  const exe = spec.exe
  const already = ['ffmpeg', 'ffprobe'].every((n) => existsSync(join(OUT, `${n}-${target}${exe}`)))
  // What the sidecars presently there actually are. A release must ship the
  // downloaded, licence-cleared static build; the development fallback is the
  // machine's own FFmpeg, dynamically linked against libraries a user's
  // computer does not have, so shipping one produces an app that cannot
  // broadcast at all.
  const sourceFile = join(OUT, `SOURCE-${target}.txt`)
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

  if (tryDownload(target, spec, tmp)) {
    writeFileSync(join(OUT, `SOURCE-${target}.txt`), `${spec.url}\n${spec.license}\n`)
    console.log('done (downloaded)')
    return
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
  for (const name of ['ffmpeg', 'ffprobe']) install(sys[name], target, name, exe)
  writeFileSync(
    join(OUT, `SOURCE-${target}.txt`),
    `system PATH fallback: ${sys.ffmpeg}\nDEVELOPMENT ONLY - not licence-cleared for redistribution\n`,
  )
  console.log('done (system fallback)')
}

main()
