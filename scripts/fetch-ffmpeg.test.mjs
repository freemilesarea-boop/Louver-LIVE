/**
 * The download path, end to end, against a server on this machine.
 *
 * Both v1.0.0 macOS release jobs died here — the archive arrived, and the
 * script went looking in it for an ffprobe that provider has never put there.
 * Nothing offline could have caught that, because nothing offline ran the
 * download at all. A local HTTP server and two tar archives do.
 */
import { describe, it, expect, beforeAll, afterAll } from 'vitest'
import { createServer } from 'node:http'
import { spawn } from 'node:child_process'
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, rmSync, chmodSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const TARGET = 'x86_64-unknown-linux-gnu'
let work
let server
let origin
let attempts = {}

/** A tar holding just `name`, the way a provider's archive holds just ffmpeg. */
function archiveWith(names) {
  const stage = mkdtempSync(join(work, 'stage-'))
  for (const name of names) {
    const p = join(stage, name)
    writeFileSync(p, `#!/bin/sh\necho ${name}\n`)
    chmodSync(p, 0o755)
  }
  const tar = join(work, `${names.join('-')}.tar`)
  execFileSync('tar', ['-cf', tar, '-C', stage, ...names])
  return tar
}

/**
 * Run the fetch script and collect what it said.
 *
 * Spawned, not `spawnSync`: the server it downloads from is in this process,
 * and a synchronous child blocks the event loop that would answer the request
 * — the script waits for a download that cannot arrive until it exits.
 */
function fetchInto(out, extra = [], env = {}) {
  const args = ['scripts/fetch-ffmpeg.mjs', '--require-download', '--force', '--target', TARGET, '--out', out]
  return new Promise((done, fail) => {
    const child = spawn(process.execPath, [...args, ...extra], {
      encoding: 'utf8',
      env: { ...process.env, ...env },
    })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (d) => (stdout += d))
    child.stderr.on('data', (d) => (stderr += d))
    child.on('error', fail)
    child.on('close', (status) => done({ status, stdout, stderr }))
  })
}

beforeAll(async () => {
  work = mkdtempSync(join(tmpdir(), 'ffmpeg-fetch-test-'))
  const files = {
    '/ffmpeg-only.tar': archiveWith(['ffmpeg']),
    '/ffprobe-only.tar': archiveWith(['ffprobe']),
    '/both.tar': archiveWith(['ffmpeg', 'ffprobe']),
  }
  server = createServer((req, res) => {
    const url = req.url.split('?')[0]
    // Two 503s, then the archive — a provider having a bad minute, which is
    // what gyan.dev did to the v1.0.4 Windows release.
    if (url === '/flaky.tar') {
      attempts.flaky = (attempts.flaky ?? 0) + 1
      if (attempts.flaky <= 2) {
        res.writeHead(503).end()
        return
      }
      res.writeHead(200, { 'content-type': 'application/x-tar' }).end(readFileSync(files['/both.tar']))
      return
    }
    if (url === '/always-503.tar') {
      attempts.down = (attempts.down ?? 0) + 1
      res.writeHead(503).end()
      return
    }
    if (url === '/gone.tar') {
      attempts.gone = (attempts.gone ?? 0) + 1
      res.writeHead(404).end()
      return
    }
    const file = files[url]
    if (!file) {
      res.writeHead(404).end()
      return
    }
    res.writeHead(200, { 'content-type': 'application/x-tar' }).end(readFileSync(file))
  })
  await new Promise((done) => server.listen(0, '127.0.0.1', done))
  origin = `http://127.0.0.1:${server.address().port}`
})

afterAll(() => {
  server?.close()
  rmSync(work, { recursive: true, force: true })
})

describe('placing the sidecars from a real download', () => {
  it('takes ffprobe from its own archive when the provider ships them apart', async () => {
    const out = join(work, 'out-split')
    mkdirSync(out, { recursive: true })
    const r = await fetchInto(out, ['--url', `${origin}/ffmpeg-only.tar`, '--probe-url', `${origin}/ffprobe-only.tar`])
    expect(r.stdout + r.stderr).toContain('done (downloaded)')
    expect(r.status).toBe(0)
    expect(existsSync(join(out, `ffmpeg-${TARGET}`))).toBe(true)
    expect(existsSync(join(out, `ffprobe-${TARGET}`))).toBe(true)
  })

  it('records both archives it used, so a release can account for what it ships', async () => {
    const out = join(work, 'out-source')
    mkdirSync(out, { recursive: true })
    await fetchInto(out, ['--url', `${origin}/ffmpeg-only.tar`, '--probe-url', `${origin}/ffprobe-only.tar`])
    const source = readFileSync(join(out, `SOURCE-${TARGET}.txt`), 'utf8')
    expect(source).toContain('/ffmpeg-only.tar')
    expect(source).toContain('/ffprobe-only.tar')
    expect(source).not.toContain('DEVELOPMENT ONLY')
  })

  it('takes both from one archive when that is how they come', async () => {
    const out = join(work, 'out-both')
    mkdirSync(out, { recursive: true })
    const r = await fetchInto(out, ['--url', `${origin}/both.tar`, '--probe-url', `${origin}/both.tar`])
    expect(r.status).toBe(0)
    for (const tool of ['ffmpeg', 'ffprobe']) {
      expect(existsSync(join(out, `${tool}-${TARGET}`)), tool).toBe(true)
    }
    // One URL, listed once.
    const source = readFileSync(join(out, `SOURCE-${TARGET}.txt`), 'utf8')
    expect(source.split('\n').filter((l) => l.includes('both.tar'))).toHaveLength(1)
  })

  it('fails, and says which archive was short, when ffprobe is not where it looked', async () => {
    // The v1.0.0 macOS failure exactly: the download works and the release
    // must still stop, rather than bundle an installer with no ffprobe.
    const out = join(work, 'out-missing')
    mkdirSync(out, { recursive: true })
    const r = await fetchInto(out, ['--url', `${origin}/ffmpeg-only.tar`, '--probe-url', `${origin}/ffmpeg-only.tar`])
    expect(r.status).toBe(1)
    expect(r.stdout).toContain('ffprobe was not in')
    expect(r.stdout).toContain('/ffmpeg-only.tar')
    expect(existsSync(join(out, `ffprobe-${TARGET}`))).toBe(false)
  })

  it('refuses to fall back to the machine\'s own FFmpeg when a release asked for the download', async () => {
    const out = join(work, 'out-404')
    mkdirSync(out, { recursive: true })
    const r = await fetchInto(out, ['--url', `${origin}/nothing-here.tar`])
    expect(r.status).toBe(1)
    expect(r.stderr).toContain('--require-download')
    expect(existsSync(join(out, `ffmpeg-${TARGET}`))).toBe(false)
  })
})

/**
 * The v1.0.4 Windows release failed here and nowhere else.
 *
 *   downloading https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip
 *   download unavailable: curl: (22) The requested URL returned error: 503
 *   FAILED: no static build could be downloaded, and --require-download was set.
 *
 * 1.2 seconds, one attempt, no retry — for a status whose whole meaning is
 * "try again". The same URL had served the v1.0.3 release ninety minutes
 * earlier. What follows is the line between a provider to wait for and a URL
 * that is simply wrong; getting it backwards either ships nothing or wastes
 * minutes on a 404.
 */
describe('a provider having a bad minute', () => {
  it('waits and asks again when the answer was 503, and ships what finally arrives', async () => {
    const out = join(work, 'out-flaky')
    mkdirSync(out, { recursive: true })
    attempts.flaky = 0
    const r = await fetchInto(
      out,
      ['--url', `${origin}/flaky.tar`, '--probe-url', `${origin}/flaky.tar`],
      { LOUVER_FETCH_BACKOFF: '0,1,1' },
    )
    expect(r.stdout + r.stderr).toContain('done (downloaded)')
    expect(r.status).toBe(0)
    expect(r.stdout).toContain('503')
    expect(r.stdout).toContain('retryable')
    expect(r.stdout).toContain('retrying in 1s')
    for (const tool of ['ffmpeg', 'ffprobe']) {
      expect(existsSync(join(out, `${tool}-${TARGET}`)), tool).toBe(true)
    }
    // The provenance is the real archive, not a note that something went wrong.
    const source = readFileSync(join(out, `SOURCE-${TARGET}.txt`), 'utf8')
    expect(source).toContain('/flaky.tar')
    expect(source).not.toContain('DEVELOPMENT ONLY')
  }, 30000)

  it('gives up honestly, and does not ship the system FFmpeg, when it stays down', async () => {
    const out = join(work, 'out-down')
    mkdirSync(out, { recursive: true })
    attempts.down = 0
    const r = await fetchInto(
      out,
      ['--url', `${origin}/always-503.tar`, '--probe-url', `${origin}/always-503.tar`],
      { LOUVER_FETCH_BACKOFF: '0,1,1' },
    )
    expect(r.status).toBe(1)
    expect(r.stderr).toContain('--require-download')
    expect(existsSync(join(out, `ffmpeg-${TARGET}`))).toBe(false)
    // Exactly as many attempts as there are waits, because the script is now
    // the only thing retrying. One would be the bug; forever would be worse.
    expect(attempts.down).toBe(3)
    expect(r.stdout).toContain('retrying in 1s (attempt 2 of 3)')
    expect(r.stdout).toContain('retrying in 1s (attempt 3 of 3)')
  }, 30000)

  it('does not wait on a 404, because the URL is wrong however long you wait', async () => {
    const out = join(work, 'out-gone')
    mkdirSync(out, { recursive: true })
    attempts.gone = 0
    const r = await fetchInto(
      out,
      ['--url', `${origin}/gone.tar`, '--probe-url', `${origin}/gone.tar`],
      // Minutes, so a retry here would be unmistakable in the elapsed time.
      { LOUVER_FETCH_BACKOFF: '0,600,600' },
    )
    expect(r.status).toBe(1)
    expect(r.stdout).not.toContain('retryable')
    expect(r.stdout).not.toContain('retrying in')
    expect(attempts.gone).toBe(1)
  }, 20000)
})
