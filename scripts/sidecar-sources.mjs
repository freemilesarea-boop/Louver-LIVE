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
  // A *release* build, deliberately. BtbN's GitHub-hosted builds were tried
  // as a second source, because johnvansickle rate-limits — but their `latest`
  // is a master snapshot, and `looped_stream_copy_does_not_accumulate_av_drift`
  // fails against it every run: one stall in thirty loop boundaries. For a
  // playlist that loops all night that is the whole product, so the nightly is
  // not an acceptable fallback and there is no second source. If this download
  // fails, the build fails and says so — CI installs no system FFmpeg on Linux
  // either, since ubuntu-22.04's is 4.4 and has no `-fps_mode` at all.
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

export const TOOLS = ['ffmpeg', 'ffprobe']

/**
 * Which archive each tool must be downloaded from, keyed by tool.
 *
 * `spec.url` may be overridden — `main()` retries with `fallbackUrl` — but the
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
