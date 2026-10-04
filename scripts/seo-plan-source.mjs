/**
 * Reads the plans out of the server's own source.
 *
 * The public pages carry prices and limits in their HTML, written at build time
 * when there is no server to ask. `SEED_PLANS` in `crates/louver-cloud/src/db.rs`
 * is where those numbers really live — it is what seeds the table the API serves
 * and what PayApp is asked to charge. So the check that the page is honest is a
 * comparison against that file, parsed here.
 *
 * Deliberately a parser and not a copy: a copy is a second source of truth, and
 * the point of this module is that there is only one.
 */
import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const PLAN_SOURCE = 'crates/louver-cloud/src/db.rs'

/**
 * The repository root.
 *
 * Worked out from this module's own location when there is one. Under vitest
 * there is not — the module is transformed and served, so `import.meta.url` is
 * not a file URL — and the caller passes the root in instead.
 */
function defaultRoot() {
  try {
    return resolve(fileURLToPath(new URL('.', import.meta.url)), '..')
  } catch {
    return process.cwd()
  }
}

/** `15 * 1024 * 1024 * 1024` → 16106127360. `19_900` → 19900. */
function evaluateNumber(text) {
  const parts = text.split('*').map((p) => Number(p.trim().replace(/_/g, '')))
  if (parts.some((n) => !Number.isFinite(n))) return null
  return parts.reduce((a, b) => a * b, 1)
}

/**
 * Every seeded plan, by id: `{ label, monthly_price_krw, active, limits }`.
 *
 * Limits are in the units the Rust file writes them in — bytes for the storage
 * keys — because converting here would be the place a rounding error hides.
 */
export async function seedPlans(root = defaultRoot()) {
  const rust = await readFile(resolve(root, PLAN_SOURCE), 'utf8')
  const start = rust.indexOf('const SEED_PLANS')
  if (start < 0) throw new Error(`no SEED_PLANS in ${PLAN_SOURCE}`)
  const end = rust.indexOf('\n];', start)
  if (end < 0) throw new Error(`SEED_PLANS in ${PLAN_SOURCE} is not terminated`)
  const body = rust.slice(start, end)

  const plans = {}
  for (const block of body.split('SeedPlan {').slice(1)) {
    const id = block.match(/id:\s*"([^"]+)"/)?.[1]
    if (!id) continue // the unsubscribed placeholder names a constant, not a literal
    const label = block.match(/label:\s*"([^"]*)"/)?.[1] ?? ''
    const price = evaluateNumber(block.match(/monthly_price_krw:\s*([^,]+),/)?.[1] ?? '')
    const active = /active:\s*true/.test(block)
    const limits = {}
    for (const [, key, value] of block.matchAll(/\("([a-z_]+)",\s*([^)]+)\)/g)) {
      const n = evaluateNumber(value)
      if (n !== null) limits[key] = n
    }
    plans[id] = { label, monthly_price_krw: price, active, limits }
  }
  if (Object.keys(plans).length === 0) throw new Error(`parsed no plans from ${PLAN_SOURCE}`)
  return plans
}

export const GIB = 1024 * 1024 * 1024
