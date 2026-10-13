// `deploy-web.sh`, exercised on a temporary directory.
//
// The full check with a real Caddy in front is `scripts/web-split-check.sh`;
// this suite is the part that belongs in CI — the release layout, the atomic
// symlink swap, the rollback, and the refusals that stop a broken bundle or a
// prune from taking the release the site is serving.
import { describe, expect, it, beforeEach, afterEach } from 'vitest'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync, readlinkSync, readdirSync, statSync, symlinkSync, renameSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'

const script = resolve(import.meta.dirname, 'deploy-web.sh')

// The script releases the web bundle on the Linux host that runs the service:
// it needs `mv -T` (which BSD/macOS `mv` does not have) and real symlinks
// (which Git Bash on Windows silently turns into copies). Running it on the
// other two CI runners would be testing their shells, not this tool — so the
// behaviour is exercised on Linux and the one platform-independent guarantee,
// that the script cannot reach a container, is checked everywhere.
const onHost = process.platform === 'linux'
const host = onHost ? describe : describe.skip

let work
let root

/** A directory shaped enough like `apps/web/dist` for the script to accept it. */
function bundle(label) {
  const dir = join(work, `dist-${label}`)
  mkdirSync(join(dir, 'assets'), { recursive: true })
  writeFileSync(join(dir, 'index.html'), `<!doctype html><p>${label}</p>`)
  writeFileSync(join(dir, 'assets', `app-${label}.js`), `// ${label}\n`)
  return dir
}

function run(...args) {
  return execFileSync('bash', [script, ...args, '--root', root], { encoding: 'utf8' }).trim()
}

beforeEach(() => {
  work = mkdtempSync(join(tmpdir(), 'deploy-web-'))
  root = join(work, 'web')
})
afterEach(() => rmSync(work, { recursive: true, force: true }))

host('publish', () => {
  it('puts the bundle under releases/ and names the version on stdout', () => {
    const version = run('publish', bundle('a'), '--version', 'v1')
    expect(version).toBe('v1')
    expect(existsSync(join(root, 'releases/v1/index.html'))).toBe(true)
    // Publishing is not releasing: nothing is being served yet.
    expect(existsSync(join(root, 'current'))).toBe(false)
  })

  it('invents a sortable version when none is given', () => {
    const version = run('publish', bundle('a'))
    expect(version).toMatch(/^\d{8}T\d{6}Z(-[0-9a-f]{7,})?$/)
  })

  it('refuses a directory that is not a built bundle', () => {
    const empty = join(work, 'empty')
    mkdirSync(empty)
    expect(() => run('publish', empty)).toThrow(/no index\.html/)
    mkdirSync(join(empty, 'assets'))
    writeFileSync(join(empty, 'index.html'), 'x')
    expect(() => run('publish', empty)).not.toThrow()
  })

  it('refuses a directory with no assets, which would serve a blank page', () => {
    const half = join(work, 'half')
    mkdirSync(half)
    writeFileSync(join(half, 'index.html'), 'x')
    expect(() => run('publish', half)).toThrow(/no assets/)
  })

  it('will not silently replace a version that already exists', () => {
    run('publish', bundle('a'), '--version', 'v1')
    expect(() => run('publish', bundle('b'), '--version', 'v1')).toThrow(/already exists/)
    expect(() => run('publish', bundle('b'), '--version', 'v1', '--force')).not.toThrow()
  })

  it('leaves nothing half-copied behind', () => {
    run('publish', bundle('a'), '--version', 'v1')
    expect(readdirSync(join(root, 'releases'))).toEqual(['v1'])
  })
})

host('activate', () => {
  it('points current at the release with a relative link', () => {
    run('publish', bundle('a'), '--version', 'v1')
    run('activate', 'v1')
    // Relative on purpose: the same link has to resolve on the host and under
    // the container's /srv/web, which are different absolute paths.
    expect(readlinkSync(join(root, 'current'))).toBe('releases/v1')
  })

  it('remembers what it replaced', () => {
    run('release', bundle('a'), '--version', 'v1')
    run('release', bundle('b'), '--version', 'v2')
    expect(readlinkSync(join(root, 'current'))).toBe('releases/v2')
    expect(readlinkSync(join(root, 'previous'))).toBe('releases/v1')
  })

  it('refuses a version that was never published', () => {
    expect(() => run('activate', 'v9')).toThrow(/no such release/)
  })

  it('is a no-op when the release is already current', () => {
    run('release', bundle('a'), '--version', 'v1')
    run('activate', 'v1')
    expect(readlinkSync(join(root, 'current'))).toBe('releases/v1')
    expect(existsSync(join(root, 'previous'))).toBe(false)
  })
})

host('rollback', () => {
  it('goes back to the release that was current before', () => {
    run('release', bundle('a'), '--version', 'v1')
    run('release', bundle('b'), '--version', 'v2')
    run('rollback')
    expect(readlinkSync(join(root, 'current'))).toBe('releases/v1')
    // And back again, because a rollback that cannot be undone is a trap.
    expect(readlinkSync(join(root, 'previous'))).toBe('releases/v2')
    run('rollback')
    expect(readlinkSync(join(root, 'current'))).toBe('releases/v2')
  })

  it('says so rather than guessing when there is nothing to go back to', () => {
    run('release', bundle('a'), '--version', 'v1')
    expect(() => run('rollback')).toThrow(/nothing to roll back to/)
  })
})

host('list', () => {
  it('marks the current and the previous release', () => {
    run('release', bundle('a'), '--version', '20260101T000000Z')
    run('release', bundle('b'), '--version', '20260102T000000Z')
    const lines = run('list').split('\n')
    expect(lines[0]).toBe('* 20260102T000000Z')
    expect(lines[1]).toBe('- 20260101T000000Z')
  })
})

host('prune', () => {
  it('keeps the newest N and deletes the rest', () => {
    for (const n of ['1', '2', '3', '4', '5']) {
      run('publish', bundle(`v${n}`), '--version', `2026010${n}T000000Z`)
    }
    run('activate', '20260105T000000Z')
    run('prune', '--keep', '2')
    expect(readdirSync(join(root, 'releases')).sort()).toEqual([
      '20260104T000000Z',
      '20260105T000000Z',
    ])
    expect(readdirSync(join(root, 'meta')).sort()).toEqual([
      '20260104T000000Z',
      '20260105T000000Z',
    ])
  })

  it('never deletes what is being served, whatever the ordering says', () => {
    run('release', bundle('a'), '--version', '20260101T000000Z')
    for (const n of ['2', '3', '4']) {
      run('publish', bundle(`v${n}`), '--version', `2026010${n}T000000Z`)
    }
    // The oldest name is the live one here, so the newest-N rule alone would
    // take the site down.
    run('prune', '--keep', '2')
    expect(existsSync(join(root, 'releases/20260101T000000Z'))).toBe(true)
  })

  it('refuses a keep count that would leave no rollback target', () => {
    expect(() => run('prune', '--keep', '1')).toThrow(/at least 2/)
  })
})


host('carrying the other build forward', () => {
  // A browser still holding the outgoing build's index.html asks for a chunk
  // this release never built. Without the hand-over that is a 404 and a blank
  // screen, which is the one failure a hashed-asset deploy cannot recover from
  // on its own.
  it('hands the outgoing release its files, as hard links', () => {
    run('release', bundle('a'), '--version', 'v1')
    run('release', bundle('b'), '--version', 'v2')
    const carried = join(root, 'releases/v2/assets/app-a.js')
    expect(existsSync(carried)).toBe(true)
    // A hard link: the bytes exist once however many releases reference them.
    expect(statSync(carried).nlink).toBeGreaterThanOrEqual(2)
    // And it did not touch what this release serves under its own names.
    expect(readFileSync(join(root, 'releases/v2/index.html'), 'utf8')).toContain('b')
  })

  it('carries in the other direction on a rollback', () => {
    run('release', bundle('a'), '--version', 'v1')
    run('release', bundle('b'), '--version', 'v2')
    run('rollback')
    // Whoever loaded v2 a second before the rollback keeps working.
    expect(existsSync(join(root, 'releases/v1/assets/app-b.js'))).toBe(true)
  })

  it('accumulates, so three builds all stay reachable', () => {
    run('release', bundle('a'), '--version', 'v1')
    run('release', bundle('b'), '--version', 'v2')
    run('release', bundle('c'), '--version', 'v3')
    expect(readdirSync(join(root, 'releases/v3/assets')).sort()).toEqual([
      'app-a.js',
      'app-b.js',
      'app-c.js',
    ])
  })

  it('keeps the bytes when the release they came from is pruned away', () => {
    run('release', bundle('a'), '--version', '20260101T000000Z')
    run('release', bundle('b'), '--version', '20260102T000000Z')
    run('release', bundle('c'), '--version', '20260103T000000Z')
    run('prune', '--keep', '2')
    expect(existsSync(join(root, 'releases/20260101T000000Z'))).toBe(false)
    // The oldest release directory is gone; its file is still served, because
    // the newest release holds a link to the same inode.
    const carried = join(root, 'releases/20260103T000000Z/assets/app-a.js')
    expect(existsSync(carried)).toBe(true)
    expect(readFileSync(carried, 'utf8')).toContain('a')
  })

  it('can be turned off, and then the other build 404s', () => {
    run('release', bundle('a'), '--version', 'v1')
    run('publish', bundle('b'), '--version', 'v2')
    run('activate', 'v2', '--no-carry')
    expect(existsSync(join(root, 'releases/v2/assets/app-a.js'))).toBe(false)
  })
})

host('check — the gate in front of the Caddy switch', () => {
  // Pointing Caddy at this directory with no release live does not fail loudly:
  // Caddy starts, the API keeps answering, and every static path returns 404.
  // So the switch is only ever made after this exits 0.
  const check = () => execFileSync('bash', [script, 'check', '--root', root], { encoding: 'utf8' })

  it('refuses an empty releases directory', () => {
    expect(check).toThrow(/NOT READY/)
  })

  it('refuses a release that is staged but not live', () => {
    run('publish', bundle('a'), '--version', 'v1')
    expect(check).toThrow(/no current link/)
  })

  it('passes once a release is live', () => {
    run('release', bundle('a'), '--version', 'v1')
    const out = check()
    expect(out).toContain('ready: releases/v1')
    expect(out).not.toContain('FAIL')
  })

  it('refuses a dangling current', () => {
    run('release', bundle('a'), '--version', 'v1')
    rmSync(join(root, 'releases/v1'), { recursive: true })
    expect(check).toThrow(/dangling/)
  })

  it('refuses an absolute current, which would not resolve inside the container', () => {
    run('release', bundle('a'), '--version', 'v1')
    const tmp = join(root, '.abs')
    symlinkSync(join(root, 'releases/v1'), tmp)
    renameSync(tmp, join(root, 'current'))
    expect(check).toThrow(/ABSOLUTE/)
  })

  it('says when there is no rollback target yet, without failing', () => {
    run('release', bundle('a'), '--version', 'v1')
    expect(check()).toContain('rollback has nowhere to go')
  })
})



describe('the delivery manifest', () => {
  // The installer refuses a staging tree whose bytes do not match its own SHAS
  // block, which makes that block the definition of what gets installed — and
  // makes it silently wrong the moment one of those files is edited without it.
  // docs/WEB_DEPLOY.md is itself in the list, so this is not a hypothetical.
  it('matches the files it claims to install', () => {
    const installer = readFileSync(resolve(import.meta.dirname, 'install-web-split.sh'), 'utf8')
    const block = installer.match(/SHAS="([^"]*)"/)
    expect(block, 'no SHAS block in install-web-split.sh').toBeTruthy()
    const rows = block[1]
      .trim()
      .split('\n')
      .map((line) => line.trim().split(/\s+/))
    expect(rows.length).toBe(6)
    const repo = resolve(import.meta.dirname, '..')
    for (const [want, file] of rows) {
      const actual = createHash('sha256').update(readFileSync(resolve(repo, file))).digest('hex')
      expect(actual, `${file} changed without updating the SHAS block in scripts/install-web-split.sh`).toBe(want)
    }
  })
})

describe('install-web-split.sh — what it is not allowed to do', () => {
  const installer = resolve(import.meta.dirname, 'install-web-split.sh')
  const code = () =>
    readFileSync(installer, 'utf8')
      .split('\n')
      .filter((line) => !line.trimStart().startsWith('#'))
      .join('\n')

  it('invokes no container, service or signal command', () => {
    // It names `docker-compose.yml` — that is the file it checks for to make
    // sure it was pointed at a deployment directory — so the assertion is about
    // invocations, not the word.
    const src = code()
    expect(src).not.toMatch(/\bdocker\s+(compose|run|exec|start|stop|restart|rm|kill|build)\b/)
    for (const forbidden of ['systemctl', 'pkill', 'killall', 'kill -', 'sqlite', 'ffmpeg']) {
      expect(src).not.toContain(forbidden)
    }
  })

  it('guards every path it removes against an empty variable', () => {
    // `rm -f -- "$a/$b"` becomes `rm -f -- /` when both are unset. Every
    // removal in a recovery tool has to say `${a:?}` so the shell stops
    // instead.
    for (const line of code().split('\n')) {
      if (!/^\s*(rm|rmdir)\s/.test(line)) continue
      expect(line, `unguarded removal: ${line.trim()}`).toMatch(/\$\{[A-Za-z_][A-Za-z0-9_]*:\?\}/)
    }
  })

  it('writes the journal ahead of the filesystem, and flushes it', () => {
    // A crash between the two has to be decidable, which needs the intent line
    // (PLAN) before the copy and the fact line (FILE) after the rename.
    const src = code()
    expect(src).toMatch(/journal "PLAN /)
    expect(src).toMatch(/journal "FILE /)
    expect(src).toMatch(/sync "\$JOURNAL"/)
  })

  it.skipIf(!onHost)('refuses an unknown subcommand', () => {
    expect(() => execFileSync('bash', [installer, 'deploy'], { encoding: 'utf8' })).toThrow(/usage/)
  })
})

describe('what it is not allowed to do', () => {
  it('contains no container, service or signal command', () => {
    // The `louver` container is the parent of every FFmpeg process on that
    // machine, and a paying customer is live in each one. A web release has no
    // business being able to reach it, so the absence is a test rather than a
    // comment.
    const source = readFileSync(script, 'utf8')
    const code = source
      .split('\n')
      .filter((line) => !line.trimStart().startsWith('#'))
      .join('\n')
    for (const forbidden of ['docker', 'systemctl', 'service ', 'pkill', 'killall', 'kill -', 'sqlite']) {
      expect(code).not.toContain(forbidden)
    }
  })

  it.skipIf(!onHost)('rejects an unknown command instead of doing something close to it', () => {
    expect(() => run('deploy')).toThrow(/unknown command/)
  })
})
