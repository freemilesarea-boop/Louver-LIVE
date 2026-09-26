#!/usr/bin/env node
/**
 * Run the whole cloud on this machine, with one command.
 *
 *   npm run cloud:dev                    # build everything and serve it
 *   npm run cloud:user -- me@example.com # create an account (Business)
 *
 * Everything the server needs lives in `.louver-dev/`, which is gitignored:
 * the database, the uploaded and prepared videos, and the master key that
 * seals stream keys. The key is generated once and kept, because a new key
 * every run would make yesterday's saved stream key unreadable.
 *
 * This is LOCAL DEVELOPMENT. The server is a process on this computer, so
 * closing the laptop ends the broadcast — which is correct, and is exactly the
 * difference between this and a Linux VPS. See docs/CLOUD_TESTING.md.
 */
import { spawn, spawnSync } from 'node:child_process'
import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { randomBytes } from 'node:crypto'
import { createInterface } from 'node:readline'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const DEV = join(ROOT, '.louver-dev')
const HOST = process.env.LOUVER_DEV_HOST ?? '127.0.0.1'
const PORT = process.env.LOUVER_DEV_PORT ?? '8080'
const args = process.argv.slice(2)
const creatingUser = args.includes('--create-user')

function run(cmd, argv, opts = {}) {
  const r = spawnSync(cmd, argv, { cwd: ROOT, stdio: 'inherit', ...opts })
  if (r.status !== 0) {
    console.error(`\n[cloud-dev] 실패: ${cmd} ${argv.join(' ')}`)
    process.exit(r.status ?? 1)
  }
}

/** One key, kept. Losing it means every saved stream key becomes unreadable. */
function masterKey() {
  mkdirSync(DEV, { recursive: true })
  const path = join(DEV, 'master.key')
  if (!existsSync(path)) {
    writeFileSync(path, randomBytes(32).toString('hex'), { mode: 0o600 })
    console.log('[cloud-dev] 새 master key를 .louver-dev/master.key 에 만들었습니다 (개발 전용)')
  }
  chmodSync(path, 0o600)
  return readFileSync(path, 'utf8').trim()
}

function env() {
  return {
    ...process.env,
    LOUVER_MASTER_KEY: masterKey(),
    LOUVER_DATA_DIR: join(DEV, 'data'),
    LOUVER_WEB_DIR: join(ROOT, 'apps/web/dist'),
    LOUVER_FFMPEG_DIR: join(ROOT, 'apps/desktop/src-tauri/binaries'),
    LOUVER_BIND: `${HOST}:${PORT}`,
    // This build is served over plain http://localhost, and a Secure cookie
    // would never be sent back. Local development only — the container never
    // sets this.
    LOUVER_INSECURE_COOKIES: '1',
    LOUVER_DEPLOYMENT: 'local',
  }
}

/** Ask for a password without printing it. */
function askPassword() {
  return new Promise((done) => {
    const rl = createInterface({ input: process.stdin, output: process.stdout, terminal: true })
    process.stdout.write('비밀번호 (10자 이상): ')
    rl.output.write = () => {} // swallow the echo
    rl.question('', (answer) => {
      rl.close()
      process.stdout.write('\n')
      done(answer.trim())
    })
  })
}

const BIN = join(ROOT, 'target/release/louver-server')

// FFmpeg first: it is what the server checks at boot, and fetching it is a
// no-op once the sidecars are in place.
run('node', ['scripts/fetch-ffmpeg.mjs'])
console.log('[cloud-dev] 서버를 빌드합니다 (처음에는 몇 분 걸립니다)…')
run('cargo', ['build', '--release', '-p', 'louver-server'])

if (creatingUser) {
  const password = process.env.LOUVER_BOOTSTRAP_PASSWORD || (await askPassword())
  if (password.length < 10) {
    console.error('[cloud-dev] 비밀번호가 너무 짧습니다.')
    process.exit(1)
  }
  // Through the environment, not the command line: arguments are visible to
  // every process on the machine and land in shell history.
  run(BIN, args, { env: { ...env(), LOUVER_BOOTSTRAP_PASSWORD: password } })
  process.exit(0)
}

console.log('[cloud-dev] 웹 화면을 빌드합니다…')
run('npm', ['run', '--silent', 'build:cloud'])

console.log('')
console.log('──────────────────────────────────────────────')
console.log('  LOCAL DEVELOPMENT')
console.log(`  http://${HOST === '0.0.0.0' ? 'localhost' : HOST}:${PORT}`)
console.log('  이 컴퓨터를 끄거나 이 창을 닫으면 방송도 끝납니다.')
console.log('──────────────────────────────────────────────')
console.log('')

const server = spawn(BIN, [], { cwd: ROOT, stdio: 'inherit', env: env() })
const stop = () => server.kill('SIGINT')
process.on('SIGINT', stop)
process.on('SIGTERM', stop)
server.on('exit', (code) => process.exit(code ?? 0))
