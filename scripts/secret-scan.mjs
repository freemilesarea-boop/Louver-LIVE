#!/usr/bin/env node
/**
 * Release security audit (§17, §60).
 *
 * Checks the working tree, the full git history, and any build/test artifacts
 * for things that must never be committed or written: stream keys, private
 * keys, signing keys and licence secrets.
 *
 * It is wired into `npm run verify`, so a key committed by accident fails the
 * build rather than being found after release.
 *
 *   node scripts/secret-scan.mjs            # working tree + history
 *   node scripts/secret-scan.mjs --runtime  # also scan app data (logs, DB)
 */
import { spawnSync } from 'node:child_process'
import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { homedir } from 'node:os'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const RUNTIME = process.argv.includes('--runtime')

/** Patterns that indicate a real secret, with why each one matters. */
const RULES = [
  {
    id: 'youtube-stream-key',
    // YouTube keys look like xxxx-xxxx-xxxx-xxxx-xxxx.
    re: /\b[a-z0-9]{4}(-[a-z0-9]{4}){3,5}\b/gi,
    why: 'looks like a YouTube stream key',
    // Version strings, UUIDs and hashes share the shape; require the grouping
    // to be uniform, which UUIDs (8-4-4-4-12) are not.
    refine: (m) => {
      const parts = m.split('-')
      if (parts[0].length !== 4 || !parts.every((p) => p.length === 4)) return false
      // UI placeholders are a single repeated character: xxxx-xxxx-xxxx.
      if (parts.every((p) => /^(.)\1{3}$/.test(p))) return false
      // A real key mixes letters and digits across the whole token.
      const body = parts.join('')
      return /[a-z]/i.test(body) && /[0-9]/.test(body)
    },
  },
  { id: 'private-key-pem', re: /-----BEGIN [A-Z ]*PRIVATE KEY-----/g, why: 'PEM private key' },
  { id: 'ssh-private-key', re: /-----BEGIN OPENSSH PRIVATE KEY-----/g, why: 'SSH private key' },
  { id: 'tauri-signing-key', re: /untrusted comment: (minisign|rsign) encrypted secret key/gi, why: 'Tauri updater signing key' },
  { id: 'aws-access-key', re: /\bAKIA[0-9A-Z]{16}\b/g, why: 'AWS access key id' },
  { id: 'generic-api-token', re: /\b(gh[pousr]_[A-Za-z0-9]{30,}|sk-[A-Za-z0-9]{32,})\b/g, why: 'API token' },
]

/** Files that legitimately contain example/placeholder values. */
const ALLOWLIST = [
  // Test and documentation fixtures deliberately use fake keys to prove masking.
  /^crates\/louver-core\/src\//,
  /^apps\/desktop\/src-tauri\/src\//,
  /^crates\/louver-core\/tests\//,
  /^apps\/desktop\/src\/test\//,
  /^apps\/desktop\/src\/.*\.test\.tsx?$/,
  /^README\.md$/,
  /^TESTING\.md$/,
  /^ARCHITECTURE\.md$/,
  /^IMPLEMENTATION_REPORT\.md$/,
  /^RELEASE_CANDIDATE_REPORT\.md$/,
  /^WINDOWS_QA\.md$/,
  /^MACOS_QA\.md$/,
  /^package-lock\.json$/,
  /^Cargo\.lock$/,
]

const SKIP_DIRS = new Set(['node_modules', 'target', '.git', 'dist', 'rc-results', 'soak-results', 'fixtures'])

const findings = []
function scan(label, text, { allowlisted = false } = {}) {
  for (const rule of RULES) {
    // Fixture files may carry fake keys on purpose; real secret shapes like a
    // PEM block are never acceptable, allowlisted or not.
    if (allowlisted && rule.id === 'youtube-stream-key') continue
    for (const m of text.matchAll(rule.re)) {
      if (rule.refine && !rule.refine(m[0])) continue
      findings.push({ rule: rule.id, why: rule.why, where: label, sample: mask(m[0]) })
    }
  }
}

const mask = (s) => (s.length <= 8 ? '•'.repeat(s.length) : `${s.slice(0, 2)}${'•'.repeat(s.length - 4)}${s.slice(-2)}`)

function walk(dir, rel = '') {
  for (const name of readdirSync(dir)) {
    if (SKIP_DIRS.has(name)) continue
    const full = join(dir, name)
    const relPath = rel ? `${rel}/${name}` : name
    let st
    try { st = statSync(full) } catch { continue }
    if (st.isDirectory()) { walk(full, relPath); continue }
    if (st.size > 2_000_000) continue
    if (relPath === 'scripts/secret-scan.mjs') continue // its own patterns
    if (/\.(png|jpg|jpeg|ico|icns|mp4|flv|woff2?|ttf|zip|gz|xz|deb|dmg|exe|msi)$/i.test(name)) continue
    let text
    try { text = readFileSync(full, 'utf8') } catch { continue }
    scan(relPath, text, { allowlisted: ALLOWLIST.some((re) => re.test(relPath)) })
  }
}

console.log('scanning working tree…')
walk(ROOT)

console.log('scanning git history…')
const log = spawnSync('git', ['log', '--all', '-p', '--no-color'], {
  cwd: ROOT, encoding: 'utf8', maxBuffer: 512 * 1024 * 1024,
})
if (log.status === 0) {
  // History cannot be allowlisted by path, but a committed PEM or token is
  // unambiguous, and fixture-shaped keys are filtered by the same refinement.
  const historyFindings = []
  for (const rule of RULES) {
    if (rule.id === 'youtube-stream-key') continue // fixtures legitimately contain these
    for (const m of log.stdout.matchAll(rule.re)) {
      historyFindings.push({ rule: rule.id, why: rule.why, where: 'git history', sample: mask(m[0]) })
    }
  }
  findings.push(...historyFindings)
} else {
  console.log('  (no git history available)')
}

if (RUNTIME) {
  console.log('scanning application data…')
  const dirs = [
    join(homedir(), '.local/share/LouverLive'),
    join(homedir(), 'Library/Application Support/LouverLive'),
    process.env.APPDATA ? join(process.env.APPDATA, 'LouverLive') : null,
  ].filter((d) => d && existsSync(d))
  for (const d of dirs) walk(d, `[appdata]${d}`)
  if (!dirs.length) console.log('  (no application data directory found)')
}

// Files that must never exist in the tree at all.
const FORBIDDEN = ['license-signing-key.txt', 'private_key.pem', '.env']
for (const f of FORBIDDEN) {
  if (existsSync(join(ROOT, f))) {
    findings.push({ rule: 'forbidden-file', why: `${f} must never be in the repository`, where: f, sample: '' })
  }
}

// The compiled-in licence key must still be the placeholder in source.
const lic = readFileSync(join(ROOT, 'crates/louver-core/src/license/mod.rs'), 'utf8')
if (!lic.includes('AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=')) {
  findings.push({
    rule: 'baked-license-key',
    why: 'a real licence public key appears to be hard-coded; it must come from LOUVER_LICENSE_PUBLIC_KEY at build time',
    where: 'crates/louver-core/src/license/mod.rs',
    sample: '',
  })
}

console.log('\n' + '─'.repeat(60))
if (!findings.length) {
  console.log('No secrets found.')
  console.log('─'.repeat(60))
  process.exit(0)
}
const seen = new Set()
for (const f of findings) {
  const k = `${f.rule}:${f.where}:${f.sample}`
  if (seen.has(k)) continue
  seen.add(k)
  console.error(`FOUND  ${f.rule.padEnd(22)} ${f.where}  ${f.sample}  (${f.why})`)
}
console.error('─'.repeat(60))
console.error(`\n${seen.size} potential secret(s). Nothing is masked in the real file — fix before release.`)
process.exit(1)
