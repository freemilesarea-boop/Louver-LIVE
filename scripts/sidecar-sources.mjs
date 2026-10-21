/**
 * Where the FFmpeg sidecars come from, and what they are called once placed.
 *
 * Shared by `fetch-ffmpeg.mjs`, which writes these files, and
 * `ffmpeg-manifest.mjs`, which reads them back and blocks a release if the
 * provenance is missing. They disagreed about the name on Windows — one wrote
 * `SOURCE-x86_64-pc-windows-msvc.txt`, the other looked for
 * `SOURCE-x86_64-pc-windows-msvc.exe.txt` — and every Windows release failed
 * at "provider not recorded". One definition, so they cannot drift again.
 */

/**
 * The Linux static build, pinned to the release it comes from.
 *
 * Two values rather than one URL repeated: the version is the thing that gets
 * moved, and it has to move for both architectures at once or the two Linux
 * targets ship different FFmpegs.
 */
const JELLYFIN_VER = '7.1.1-7'
const JELLYFIN = `https://github.com/jellyfin/jellyfin-ffmpeg/releases/download/v${JELLYFIN_VER}`

/** A pinned rehost of other people's builds, used only where it is a last resort. */
const FFBINARIES = 'https://github.com/ffbinaries/ffbinaries-prebuilt/releases/download/v6.1'

/** Tauri target triples, and where an official static build comes from. */
export const SOURCES = {
  'x86_64-pc-windows-msvc': {
    url: 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip',
    archive: 'zip',
    exe: '.exe',
    license: 'GPL v3 (gyan.dev release-essentials)',
  },
  // evermeet.cx and osxexperts.net publish one download per tool. The archive
  // named "ffmpeg" holds ffmpeg and nothing else, so ffprobe needs its own URL.
  //
  // evermeet.cx is gone, not moved. `getrelease/zip` answered `HTTP 410 Gone`
  // from 2026-10-18, and naming the tool — `getrelease/ffmpeg/zip`, the form
  // the ffprobe URL beside it had always used — answers 410 as well. 410 is
  // permanent, so this job failed in about a second on every push.
  //
  // So this target moves to osxexperts.net, which is the provider the Apple
  // Silicon row below already uses and the same 7.1.1 build, with ffbinaries'
  // pinned 6.1 behind it. Both are listed because neither can be checked from
  // a Linux machine and the Intel runner builds no media in CI — the only
  // thing that executes this binary before a release is
  // `ffmpeg-manifest.mjs --check` on the Intel runner, which reads its
  // version, licence, linkage and encoders out of the binary itself.
  'x86_64-apple-darwin': {
    url: 'https://www.osxexperts.net/ffmpeg711intel.zip',
    probeUrl: 'https://www.osxexperts.net/ffprobe711intel.zip',
    alternates: [
      {
        url: `${FFBINARIES}/ffmpeg-6.1-macos-64.zip`,
        probeUrl: `${FFBINARIES}/ffprobe-6.1-macos-64.zip`,
        license: 'GPL v3 (ffbinaries pinned 6.1)',
      },
    ],
    archive: 'zip',
    exe: '',
    license: 'GPL v3 (osxexperts.net)',
  },
  'aarch64-apple-darwin': {
    url: 'https://www.osxexperts.net/ffmpeg711arm.zip',
    probeUrl: 'https://www.osxexperts.net/ffprobe711arm.zip',
    archive: 'zip',
    exe: '',
    license: 'GPL v3 (osxexperts.net)',
  },
  // johnvansickle.com stopped answering on 2026-10-19 — curl times out with
  // zero bytes received, every attempt, so each job spent twenty minutes
  // reaching the same conclusion and no Linux job has run a test since. It
  // served a moving "latest release" with no versioned URL, so there is
  // nothing of it left to pin.
  //
  // A *release* build, and that is the whole difficulty. The replacements that
  // can still be downloaded were measured against the two tests that decide
  // whether a build can run this product, and most of them fail one:
  //
  //   johnvansickle 6.0 / 6.1 static (pinned GitHub mirrors)
  //       loop drift: pass. `live_video_source`: SIGSEGV in the HLS demuxer
  //       on the second segment, every run, on three separate attempts. A
  //       camera broadcast would die on the spot.
  //   BtbN master (the only BtbN asset with a stable URL)
  //       `live_video_source`: pass. Loop drift: fails 10 of 10 — a stall at a
  //       loop boundary, which for a playlist that loops all night is the
  //       whole product. This is the same build rejected here before, and the
  //       measurement still holds.
  //   jellyfin-ffmpeg 7.1.1 portable
  //       both pass, 8 of 8 on the drift test. --enable-gpl --enable-version3,
  //       one tarball holding ffmpeg and ffprobe, and the only shared objects
  //       it needs are libc and friends, so `ffmpeg-manifest.mjs`'s linkage
  //       gate is satisfied by the binary itself.
  //
  // Hence this one. It is pinned to a tag rather than a "latest", which is
  // what made the old URL impossible to recover from: the bytes cannot change
  // under a release. Moving it is a decision about what customers run, so the
  // version is written here once and the measurements above say what has to be
  // re-measured before it moves.
  'x86_64-unknown-linux-gnu': {
    url: `${JELLYFIN}/jellyfin-ffmpeg_${JELLYFIN_VER}_portable_linux64-gpl.tar.xz`,
    archive: 'tar.xz',
    exe: '',
    license: 'GPL v3 (jellyfin-ffmpeg portable)',
  },
  'aarch64-unknown-linux-gnu': {
    url: `${JELLYFIN}/jellyfin-ffmpeg_${JELLYFIN_VER}_portable_linuxarm64-gpl.tar.xz`,
    archive: 'tar.xz',
    exe: '',
    license: 'GPL v3 (jellyfin-ffmpeg portable)',
  },
}

export const TOOLS = ['ffmpeg', 'ffprobe']

/**
 * Which archive each tool must be downloaded from, keyed by tool.
 *
 * `spec.url` may be overridden — `main()` works through `alternates` — but the
 * ffprobe archive is a property of the source, so it is read from the table.
 */
export function urlsFor(target, url) {
  const spec = SOURCES[target]
  if (!spec) return null
  const main = url ?? spec.url
  return { ffmpeg: main, ffprobe: spec.probeUrl ?? main }
}

/** What Tauri expects to find in `binaries/`: `<tool>-<triple><exe>`. */
export function sidecarName(tool, target) {
  return `${tool}-${target}${SOURCES[target]?.exe ?? ''}`
}

/** Where the provenance of a target's sidecars is recorded. Never executable. */
export function provenanceName(target) {
  return `SOURCE-${target}.txt`
}

/** The triple a placed sidecar is named for — the inverse of `sidecarName`. */
export function tripleOf(binaryName) {
  return binaryName.replace(/^(ffmpeg|ffprobe)-/, '').replace(/\.exe$/i, '')
}
