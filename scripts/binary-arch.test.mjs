/**
 * The header reader that decides whether an installer may ship a sidecar.
 *
 * Built from synthetic headers rather than real binaries: the mistake this
 * guards against — a download host serving the wrong asset — cannot be staged
 * with the binaries a runner happens to have, and the release for Windows and
 * macOS is judged by exactly this code.
 */
import { describe, it, expect } from 'vitest'
import { readFileSync, existsSync } from 'node:fs'
import { archOf, expectedArch, satisfies } from './binary-arch.mjs'

/** ELF header: 7f E L F, class, endianness, … then e_machine at 0x12. */
function elf(machine, { little = true, bits64 = true } = {}) {
  const b = Buffer.alloc(64)
  b.write('\x7fELF', 0, 'binary')
  b[4] = bits64 ? 2 : 1
  b[5] = little ? 1 : 2
  if (little) b.writeUInt16LE(machine, 0x12)
  else b.writeUInt16BE(machine, 0x12)
  return b
}

/** Mach-O header: magic then cputype. */
function macho(cpuType, magic = 0xfeedfacf) {
  const b = Buffer.alloc(32)
  b.writeUInt32LE(magic, 0)
  b.writeUInt32LE(cpuType, 4)
  return b
}

/** Universal binary: big-endian magic, a count, then 20 bytes per arch. */
function universal(cpuTypes) {
  const b = Buffer.alloc(8 + cpuTypes.length * 20)
  b.writeUInt32BE(0xcafebabe, 0)
  b.writeUInt32BE(cpuTypes.length, 4)
  cpuTypes.forEach((t, i) => b.writeUInt32BE(t, 8 + i * 20))
  return b
}

/** PE: 'MZ', the PE header offset at 0x3c, 'PE\0\0', then the machine. */
function pe(machine, at = 0x80) {
  const b = Buffer.alloc(at + 8)
  b.write('MZ', 0, 'binary')
  b.writeUInt32LE(at, 0x3c)
  b.writeUInt32LE(0x00004550, at)
  b.writeUInt16LE(machine, at + 4)
  return b
}

describe('reading a machine out of an executable header', () => {
  it('reads ELF, both word orders', () => {
    expect(archOf(elf(0x3e))).toEqual({ format: 'elf', arch: 'x86_64' })
    expect(archOf(elf(0xb7))).toEqual({ format: 'elf', arch: 'arm64' })
    expect(archOf(elf(0xb7, { little: false }))).toEqual({ format: 'elf', arch: 'arm64' })
  })

  it('reads Mach-O', () => {
    expect(archOf(macho(0x0100000c))).toEqual({ format: 'macho', arch: 'arm64' })
    expect(archOf(macho(0x01000007))).toEqual({ format: 'macho', arch: 'x86_64' })
  })

  it('reads every slice of a universal binary', () => {
    const got = archOf(universal([0x01000007, 0x0100000c]))
    expect(got.format).toBe('macho-universal')
    expect(got.arches).toEqual(['x86_64', 'arm64'])
  })

  it('reads PE', () => {
    expect(archOf(pe(0x8664))).toEqual({ format: 'pe', arch: 'x86_64' })
    expect(archOf(pe(0xaa64))).toEqual({ format: 'pe', arch: 'arm64' })
  })

  it('says nothing rather than guessing at something that is not an executable', () => {
    expect(archOf(Buffer.from('#!/bin/sh\necho hi\n'))).toBeNull()
    expect(archOf(Buffer.alloc(2))).toBeNull()
  })

  it('reads the real binary on this machine, when one is here', () => {
    const path = 'apps/desktop/src-tauri/binaries/ffmpeg-x86_64-unknown-linux-gnu'
    if (!existsSync(path)) return
    expect(archOf(readFileSync(path).subarray(0, 4096))).toEqual({ format: 'elf', arch: 'x86_64' })
  })
})

describe('what each release target needs', () => {
  it('maps the four shipping triples', () => {
    expect(expectedArch('x86_64-pc-windows-msvc')).toMatchObject({ format: 'pe', arch: 'x86_64' })
    expect(expectedArch('aarch64-apple-darwin')).toMatchObject({ format: 'macho', arch: 'arm64' })
    expect(expectedArch('x86_64-apple-darwin')).toMatchObject({ format: 'macho', arch: 'x86_64' })
    expect(expectedArch('x86_64-unknown-linux-gnu')).toMatchObject({ format: 'elf', arch: 'x86_64' })
  })

  it('reads the OS out of the triple, not the vendor', () => {
    // `x86_64-pc-windows-msvc` has `pc` where `apple` and `unknown` sit in the
    // others, so keying on that position quietly produced no expectation at
    // all for Windows — a check that cannot fail.
    expect(expectedArch('x86_64-pc-windows-msvc')).not.toBeNull()
  })

  it('has no expectation for a triple it does not know, rather than a wrong one', () => {
    expect(expectedArch('riscv64-unknown-none')).toBeNull()
  })
})

describe('deciding whether a sidecar may ship', () => {
  const win = expectedArch('x86_64-pc-windows-msvc')
  const macArm = expectedArch('aarch64-apple-darwin')
  const linux = expectedArch('x86_64-unknown-linux-gnu')

  it('accepts the right binary for the target', () => {
    expect(satisfies(archOf(pe(0x8664)), win)).toBe(true)
    expect(satisfies(archOf(macho(0x0100000c)), macArm)).toBe(true)
    expect(satisfies(archOf(elf(0x3e)), linux)).toBe(true)
  })

  it('refuses the right format for the wrong machine', () => {
    // The Intel .dmg served to the Apple Silicon build: a bundle that
    // installs, opens, and cannot start a broadcast.
    expect(satisfies(archOf(macho(0x01000007)), macArm)).toBe(false)
    expect(satisfies(archOf(pe(0xaa64)), win)).toBe(false)
  })

  it('refuses another platform\'s binary entirely', () => {
    expect(satisfies(archOf(elf(0x3e)), win)).toBe(false)
    expect(satisfies(archOf(pe(0x8664)), linux)).toBe(false)
  })

  it('accepts a universal binary that contains the target', () => {
    expect(satisfies(archOf(universal([0x01000007, 0x0100000c])), macArm)).toBe(true)
    expect(satisfies(archOf(universal([0x01000007])), macArm)).toBe(false)
  })

  it('refuses a file it could not read at all', () => {
    expect(satisfies(null, win)).toBe(false)
    expect(satisfies(archOf(pe(0x8664)), null)).toBe(false)
  })
})
