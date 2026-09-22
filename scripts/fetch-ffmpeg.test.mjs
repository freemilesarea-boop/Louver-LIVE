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
function fetchInto(out, extra = []) {
  const args = ['scripts/fetch-ffmpeg.mjs', '--require-download', '--force', '--target', TARGET, '--out', out]
  return new Promise((done, fail) => {
    const child = spawn(process.execPath, [...args, ...extra], { encoding: 'utf8' })
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
    const file = files[req.url]
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
