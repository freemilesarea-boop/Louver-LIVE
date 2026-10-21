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

/** gyan.dev's own releases, which are the same builds its web server serves. */
const GYAN_PINNED_VER = '7.1.1'
const GYAN_PINNED = 'https://github.com/GyanD/codexffmpeg/releases/download/7.1.1'

/** Tauri target triples, and where an official static build comes from. */
export const SOURCES = {
  // gyan.dev is the last provider here that is one person's web server rather
  // than a pinned release asset, and it has already failed a release: it
  // answered 503 for this exact URL during v1.0.4, ninety minutes after
  // serving v1.0.3. So the same builder's own GitHub releases stand behind it,
  // pinned — `essentials_build` is the same flavour this URL serves, at the
  // version the other targets are on. Nothing has had to use it yet; it is
  // here because the two providers that went down in October gave no warning
  // either, and because this is the retry loop's only defence that does not
  // depend on the same host coming back.
  'x86_64-pc-windows-msvc': {
    url: 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip',
    alternates: [
      {
        url: `${GYAN_PINNED}/ffmpeg-${GYAN_PINNED_VER}-essentials_build.zip`,
        license: `GPL v3 (gyan.dev essentials ${GYAN_PINNED_VER}, pinned mirror)`,
      },
    ],
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
  // So this target takes evermeet's build from the last place that still
  // serves it: ffbinaries rehosts it as an asset of a pinned release, and the
  // binary identifies itself as `6.1-tessus https://evermeet.cx/ffmpeg/` —
  // the same builder, one version back from the one the dead API served.
  // osxexperts.net was tried first, since the Apple Silicon row below uses it
  // and it is still up, but it publishes no Intel build: `ffmpeg711intel.zip`
  // is a 404. A primary that cannot ever answer is a wasted request on every
  // build, so it is not kept here.
  //
  // What this is NOT known to be is a 6.x that works. The johnvansickle 6.0
  // and 6.1 *Linux* statics segfault the camera pipeline, and whether
  // evermeet's macOS 6.1 shares that is unmeasured: nothing executes this
  // binary except `ffmpeg-manifest.mjs --check` on the Intel runner, which
  // reads its version, licence, linkage and encoders but broadcasts nothing,
  // and the `verify` job that does run the media tests on macOS runs on
  // Apple Silicon. Worth measuring on an Intel Mac before trusting a camera
  // broadcast there.
  'x86_64-apple-darwin': {
    url: `${FFBINARIES}/ffmpeg-6.1-macos-64.zip`,
    probeUrl: `${FFBINARIES}/ffprobe-6.1-macos-64.zip`,
    archive: 'zip',
    exe: '',
    license: 'GPL v3 (evermeet.cx build, ffbinaries pinned mirror)',
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
