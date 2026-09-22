/**
 * What the Windows MSI is allowed to carry.
 *
 * The v1.0.2 release built everything — Rust, the NSIS installer, both
 * macOS bundles, Linux — and then WiX refused to link the MSI over two
 * right arrows in the licence file:
 *
 *   LicenseAgreementDlg.wxs(27) : error LGHT0311 : A string was provided
 *   with characters that are not available in the specified database code
 *   page '1252'.
 *
 * Nothing before the release could have caught it, because nothing else
 * reads that file the way WiX does. This does.
 */
import { describe, it, expect } from 'vitest'
import { readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { notInCp1252, isCp1252 } from './cp1252.mjs'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const TAURI_DIR = join(ROOT, 'apps/desktop/src-tauri')
const config = JSON.parse(readFileSync(join(TAURI_DIR, 'tauri.conf.json'), 'utf8'))

describe('the code page check itself', () => {
  it('accepts what CP1252 has', () => {
    for (const ch of 'abcXYZ0129 .,/()—©§™€…“”‘’–•±¾ÿÀ') {
      expect(isCp1252(ch), ch).toBe(true)
    }
  })

  it('refuses what it does not', () => {
    // The arrow that stopped the release, and the Korean the product is
    // written in. (™ and — are not here: CP1252 does have those.)
    for (const ch of '→←↑⇒음악✓') {
      expect(isCp1252(ch), ch).toBe(false)
    }
  })
})

describe('the licence the installer shows', () => {
  const licenseFile = config.bundle?.licenseFile
  const text = licenseFile ? readFileSync(resolve(TAURI_DIR, licenseFile), 'utf8') : ''

  it('is the file the bundle config names', () => {
    expect(licenseFile, 'bundle.licenseFile is not set').toBeTruthy()
    expect(text.length).toBeGreaterThan(0)
  })

  it('holds nothing the MSI database cannot store', () => {
    const bad = notInCp1252(text)
    const detail = bad.map((b) => `${b.codepoint} ${b.char} at ${licenseFile}:${b.line} — ${b.context}`)
    expect(detail, 'WiX would fail with LGHT0311 on these').toEqual([])
  })
})

describe('the name the installer is built under', () => {
  it('is storable too', () => {
    // productName becomes the MSI's ProductName, a localizable string like
    // the licence. The descriptions are deliberately not checked: they go to
    // the Summary Information stream, which carries its own code page, and
    // they are Korean on purpose. If a future LGHT0311 ever names one of
    // them, this is where the check belongs.
    expect(notInCp1252(config.productName ?? '')).toEqual([])
  })
})
