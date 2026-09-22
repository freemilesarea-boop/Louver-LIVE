/**
 * Which linked libraries mean a binary will not run on a customer's machine.
 *
 * Both a false negative and a false positive here cost a release: one ships
 * an installer that cannot start FFmpeg, the other blocks a build that was
 * perfectly good. The second is what happened — `/usr/lib/libexpat.1.dylib`,
 * which has shipped with macOS for years, failed the Apple Silicon build.
 */
import { describe, it, expect } from 'vitest'
import { classify, isSystemLibrary } from './linkage.mjs'

/** What `otool -L` printed for the osxexperts build the release downloads. */
const OSXEXPERTS = `apps/desktop/src-tauri/binaries/ffmpeg-aarch64-apple-darwin:
\t/usr/lib/libexpat.1.dylib (compatibility version 7.0.0, current version 8.0.0)
\t/usr/lib/libbz2.1.0.dylib (compatibility version 1.0.0, current version 1.0.8)
\t/usr/lib/libiconv.2.dylib (compatibility version 7.0.0, current version 7.0.0)
\t/usr/lib/libz.1.dylib (compatibility version 1.0.0, current version 1.2.12)
\t/usr/lib/libc++.1.dylib (compatibility version 1.0.0, current version 1800.101.0)
\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1351.0.0)`

/** A build made on a developer's Mac against Homebrew. Must never ship. */
const HOMEBREW = `${OSXEXPERTS}
\t/opt/homebrew/opt/x264/lib/libx264.164.dylib (compatibility version 0.0.0, current version 0.0.0)
\t/usr/local/lib/libvpx.9.dylib (compatibility version 0.0.0, current version 0.0.0)`

/** johnvansickle's static Linux build. */
const STATIC_LINUX = '\tnot a dynamic executable'

/** Ubuntu's own dynamically linked ffmpeg — everything a user does not have. */
const APT_LINUX = `\tlinux-vdso.so.1 (0x00007ffc)
\tlibavdevice.so.58 => /lib/x86_64-linux-gnu/libavdevice.so.58 (0x00007f0)
\tlibavfilter.so.7 => /lib/x86_64-linux-gnu/libavfilter.so.7 (0x00007f1)
\tlibc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007f2)
\tlibm.so.6 => /lib/x86_64-linux-gnu/libm.so.6 (0x00007f3)`

describe('macOS', () => {
  it('counts the system libraries every Mac has as system', () => {
    // The real regression: this build is fit to ship and was rejected.
    const got = classify(OSXEXPERTS)
    expect(got.foreign).toEqual([])
    expect(got.foreign_libraries).toBe(0)
    expect(got.static).toBe(true)
    expect(got.shared_libraries).toBe(6)
  })

  it('names libexpat specifically, since that is the one that failed a release', () => {
    expect(isSystemLibrary('/usr/lib/libexpat.1.dylib (compatibility version 7.0.0, current version 8.0.0)')).toBe(true)
  })

  it('still refuses a build linked against a package manager', () => {
    const got = classify(HOMEBREW)
    expect(got.static).toBe(false)
    expect(got.foreign_libraries).toBe(2)
    expect(got.foreign.join(' ')).toContain('libx264')
    expect(got.foreign.join(' ')).toContain('libvpx')
  })

  it('judges by where a library lives, not what it is called', () => {
    // `/usr/local` and `/opt` are where a package manager puts things; a
    // library with a perfectly ordinary name there is still not on a
    // customer's machine.
    expect(isSystemLibrary('/usr/local/lib/libz.1.dylib')).toBe(false)
    expect(isSystemLibrary('@rpath/libavcodec.60.dylib')).toBe(false)
  })
})

describe('Linux', () => {
  it('accepts the static build the release ships', () => {
    const got = classify(STATIC_LINUX)
    expect(got.static).toBe(true)
    expect(got.shared_libraries).toBe(0)
  })

  it('refuses a build that needs the distribution\'s own FFmpeg libraries', () => {
    const got = classify(APT_LINUX)
    expect(got.static).toBe(false)
    expect(got.foreign.join(' ')).toContain('libavdevice')
    // libc, libm and the vdso are not held against it.
    expect(got.foreign_libraries).toBe(2)
  })
})

describe('a platform with neither otool nor ldd', () => {
  it('says nothing was determined, which is not a finding', () => {
    const got = classify('', { determined: false })
    expect(got.static).toBeNull()
    expect(got.note).toMatch(/not determined/)
  })
})
