/**
 * What machine an executable is built for, read from its own header.
 *
 * A download served the wrong asset produces an installer that looks perfect
 * and cannot spawn FFmpeg on the customer's computer, so a release checks. It
 * used to shell out to `file`, which is a package that has to be installed on
 * the Linux runner and is not part of every Git for Windows — and the Windows
 * release had never reached this check to find out. Three header formats,
 * about forty bytes each, is less risk than a tool that may not be there.
 */

/** PE machine ids (IMAGE_FILE_MACHINE_*). */
const PE = { 0x8664: 'x86_64', 0xaa64: 'arm64', 0x014c: 'x86' }
/** Mach-O cpu types, with the 64-bit flag already applied. */
const MACHO = { 0x01000007: 'x86_64', 0x0100000c: 'arm64', 0x00000007: 'x86', 0x0000000c: 'arm' }
/** ELF e_machine values. */
const ELF = { 0x3e: 'x86_64', 0xb7: 'arm64', 0x03: 'x86', 0x28: 'arm' }

/**
 * `{ format, arch }` for a buffer holding the start of an executable, or null
 * if it is not one this understands. `arch` is null when the format is known
 * but the machine is not.
 */
export function archOf(buf) {
  if (buf.length < 8) return null

  // ELF: 7f 'E' 'L' 'F', then e_machine at 0x12 in the file's own byte order.
  if (buf[0] === 0x7f && buf[1] === 0x45 && buf[2] === 0x4c && buf[3] === 0x46) {
    if (buf.length < 0x14) return { format: 'elf', arch: null }
    const little = buf[5] !== 2
    const machine = little ? buf.readUInt16LE(0x12) : buf.readUInt16BE(0x12)
    return { format: 'elf', arch: ELF[machine] ?? null }
  }

  // Mach-O: the 64-bit magic little- or big-endian, then cputype.
  const magic = buf.readUInt32LE(0)
  if (magic === 0xfeedfacf || magic === 0xfeedface) {
    return { format: 'macho', arch: MACHO[buf.readUInt32LE(4)] ?? null }
  }
  if (magic === 0xcffaedfe || magic === 0xcefaedfe) {
    return { format: 'macho', arch: MACHO[buf.readUInt32BE(4)] ?? null }
  }
  // A universal binary carries several. It is fit for the target if one of
  // them matches, so report them all.
  if (buf.readUInt32BE(0) === 0xcafebabe) {
    const count = buf.readUInt32BE(4)
    const arches = []
    for (let i = 0; i < count && 8 + i * 20 + 4 <= buf.length; i += 1) {
      arches.push(MACHO[buf.readUInt32BE(8 + i * 20)] ?? null)
    }
    return { format: 'macho-universal', arch: arches.filter(Boolean).join('+') || null, arches }
  }

  // PE: 'MZ', the PE header offset at 0x3c, then 'PE\0\0' and the machine.
  if (buf[0] === 0x4d && buf[1] === 0x5a) {
    if (buf.length < 0x40) return { format: 'pe', arch: null }
    const at = buf.readUInt32LE(0x3c)
    if (buf.length < at + 6) return { format: 'pe', arch: null }
    if (buf.readUInt32LE(at) !== 0x00004550) return { format: 'pe', arch: null }
    return { format: 'pe', arch: PE[buf.readUInt16LE(at + 4)] ?? null }
  }

  return null
}

/**
 * What a sidecar for this Tauri target triple must be.
 *
 * Read from the triple's own parts rather than a table, so a target added
 * later is covered — but only the parts that mean something. A triple is
 * `<arch>-<vendor>-<os>[-<abi>]`, and the vendor is `pc`, `apple` or
 * `unknown`, which is not what decides the executable format.
 */
export function expectedArch(target) {
  const parts = target.split('-')
  const machine = { x86_64: 'x86_64', aarch64: 'arm64' }[parts[0]]
  const os = parts.slice(1).find((p) => ['windows', 'darwin', 'linux'].includes(p))
  const format = { windows: 'pe', darwin: 'macho', linux: 'elf' }[os]
  if (!machine || !format) return null
  return { format, arch: machine, os }
}

/** Whether what was read satisfies what the target needs. */
export function satisfies(got, want) {
  if (!got || !want) return false
  const formatOk = got.format === want.format || (want.format === 'macho' && got.format === 'macho-universal')
  if (!formatOk) return false
  return got.format === 'macho-universal'
    ? (got.arches ?? []).includes(want.arch)
    : got.arch === want.arch
}
