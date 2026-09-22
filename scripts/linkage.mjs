/**
 * Whether a binary will run on someone else's computer.
 *
 * Judged by *what* is linked, not how many. Every binary links the platform
 * runtime, and counting those scored a genuinely self-contained build as
 * unfit at nine glibc entries while a build with eight codec libraries would
 * have passed. What actually decides it is whether the binary needs a library
 * that a customer's machine has no reason to have: libx264, libvpx, libssl,
 * anything a package manager put there.
 */

/**
 * Libraries every machine of that platform already has.
 *
 * On macOS the rule is the location, not the name. Everything under
 * `/usr/lib` and `/System/Library` is part of the OS and is in the dyld
 * shared cache; a package manager installs into `/opt/homebrew` or
 * `/usr/local`, which is what would not be there. Naming individual
 * libraries instead failed the osxexperts build over
 * `/usr/lib/libexpat.1.dylib`, which has shipped with macOS for years — the
 * check was right to look and wrong about the answer.
 */
export const SYSTEM = [
  // Linux: the platform runtime.
  /\blibc\b/, /\blibm\b/, /\blibdl\b/, /\blibrt\b/, /\blibpthread\b/,
  /\blibmvec\b/, /\blibgcc_s\b/, /\bld-linux/, /\blinux-vdso/, /\blibresolv\b/,
  // macOS: the OS's own directories.
  /^\/usr\/lib\//, /^\/System\/Library\//,
]

/** Is this line of `otool -L` / `ldd` output a library the platform provides? */
export function isSystemLibrary(line) {
  return SYSTEM.some((re) => re.test(line.trim()))
}

/**
 * Read `otool -L` or `ldd` output into a verdict.
 *
 * `static: null` means nothing was looked at — the platform has neither tool
 * — which is a third state and not a finding.
 */
export function classify(out, { determined = true } = {}) {
  if (!determined) {
    return { static: null, shared_libraries: null, note: 'not determined on this platform' }
  }
  if (/not a dynamic executable|statically linked/i.test(out)) {
    return { static: true, shared_libraries: 0, foreign_libraries: 0, foreign: [] }
  }
  const lines = out
    .split('\n')
    .filter((l) => /=>|\.dylib|\.so/.test(l))
    .map((l) => l.trim())
  const foreign = lines.filter((l) => !isSystemLibrary(l))
  return {
    static: foreign.length === 0,
    shared_libraries: lines.length,
    foreign_libraries: foreign.length,
    foreign: foreign.slice(0, 12),
  }
}
