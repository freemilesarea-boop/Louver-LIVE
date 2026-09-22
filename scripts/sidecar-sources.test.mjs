/**
 * The two release blockers that a tag push found and CI did not, held down.
 *
 * Both were failures of agreement between two scripts that never run together
 * outside a release: `fetch-ffmpeg.mjs` places the sidecars,
 * `ffmpeg-manifest.mjs` reads them back and refuses to ship what it cannot
 * account for. Ordinary CI runs neither in release mode, so nothing noticed.
 */
import { describe, it, expect } from 'vitest'
import { SOURCES, TOOLS, urlsFor, sidecarName, provenanceName, tripleOf } from './sidecar-sources.mjs'

/** The four platforms the product ships installers for. */
const RELEASE_TARGETS = [
  'x86_64-pc-windows-msvc',
  'aarch64-apple-darwin',
  'x86_64-apple-darwin',
  'x86_64-unknown-linux-gnu',
]

describe('where each sidecar is downloaded from', () => {
  it('asks for ffprobe separately wherever ffmpeg ships alone', () => {
    // evermeet.cx and osxexperts.net publish one archive per tool. Asking
    // both times for the ffmpeg archive is what failed the v1.0.0 macOS
    // builds: the download worked, and then "ffprobe was not in the archive".
    for (const target of ['aarch64-apple-darwin', 'x86_64-apple-darwin']) {
      const urls = urlsFor(target)
      expect(urls.ffprobe, target).toBe(SOURCES[target].probeUrl)
      expect(urls.ffprobe, target).not.toBe(urls.ffmpeg)
      expect(urls.ffprobe, target).toMatch(/ffprobe/)
    }
  })

  it('takes both tools from the one archive that holds them', () => {
    for (const target of ['x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu']) {
      const urls = urlsFor(target)
      expect(urls.ffprobe, target).toBe(urls.ffmpeg)
    }
  })

  it('names a source for every target a release builds', () => {
    for (const target of RELEASE_TARGETS) {
      const urls = urlsFor(target)
      expect(urls, target).not.toBeNull()
      for (const tool of TOOLS) expect(urls[tool], `${target} ${tool}`).toMatch(/^https:\/\//)
    }
  })

  it('honours a retry URL for ffmpeg without losing the ffprobe archive', () => {
    const alt = 'https://example.invalid/ffmpeg.zip'
    const urls = urlsFor('aarch64-apple-darwin', alt)
    expect(urls.ffmpeg).toBe(alt)
    expect(urls.ffprobe).toBe(SOURCES['aarch64-apple-darwin'].probeUrl)
  })
})

describe('what the placed sidecars are called', () => {
  it('gives Windows binaries the .exe the other platforms do not have', () => {
    expect(sidecarName('ffmpeg', 'x86_64-pc-windows-msvc')).toBe('ffmpeg-x86_64-pc-windows-msvc.exe')
    expect(sidecarName('ffprobe', 'aarch64-apple-darwin')).toBe('ffprobe-aarch64-apple-darwin')
  })

  it('records provenance under a name with no executable suffix', () => {
    // The release reads this file back to prove the binaries are the
    // licence-cleared download and not the runner's own FFmpeg. Looking for
    // `SOURCE-…-msvc.exe.txt` found nothing and blocked every Windows release
    // at "provider not recorded".
    expect(provenanceName('x86_64-pc-windows-msvc')).toBe('SOURCE-x86_64-pc-windows-msvc.txt')
    expect(provenanceName('x86_64-pc-windows-msvc')).not.toMatch(/\.exe/)
  })

  it('reads the triple back off a placed binary, on every platform', () => {
    for (const target of RELEASE_TARGETS) {
      for (const tool of TOOLS) {
        expect(tripleOf(sidecarName(tool, target)), `${tool} ${target}`).toBe(target)
      }
    }
  })

  it('finds the provenance the fetcher wrote, from the binary the manifest sees', () => {
    // The whole round trip, which is the thing that was broken: the name the
    // manifest computes from a placed binary must be the name fetch wrote.
    for (const target of RELEASE_TARGETS) {
      const written = provenanceName(target)
      const lookedUp = provenanceName(tripleOf(sidecarName('ffmpeg', target)))
      expect(lookedUp, target).toBe(written)
    }
  })
})
