/**
 * What the Windows installer database can actually hold.
 *
 * The MSI is linked for the `en-us` culture, whose localization sets the
 * database code page to 1252. Every localizable string that goes into the
 * database — the licence agreement text among them — has to be expressible
 * in it, and WiX refuses the whole build if one is not:
 *
 *   LicenseAgreementDlg.wxs(27) : error LGHT0311 : A string was provided
 *   with characters that are not available in the specified database code
 *   page '1252'.
 *
 * Two right arrows in LICENSES.md were enough to stop the v1.0.2 Windows
 * release after everything else in it had built.
 */

/** The 27 printable characters CP1252 puts in 0x80–0x9F, where Latin-1 has controls. */
const HIGH_CONTROL_RANGE = [
  '€', '', '‚', 'ƒ', '„', '…', '†', '‡',
  'ˆ', '‰', 'Š', '‹', 'Œ', '', 'Ž', '',
  '', '‘', '’', '“', '”', '•', '–', '—',
  '˜', '™', 'š', '›', 'œ', '', 'ž', 'Ÿ',
].filter(Boolean)

const EXTRA = new Set(HIGH_CONTROL_RANGE)

/** Can this one character be stored in a code page 1252 database? */
export function isCp1252(ch) {
  const code = ch.codePointAt(0)
  // ASCII and the Latin-1 upper half map straight through; 0x80–0x9F do not.
  if (code <= 0x7f || (code >= 0xa0 && code <= 0xff)) return true
  return EXTRA.has(ch)
}

/**
 * Every character of `text` that the database cannot hold, with where it is.
 * Empty means the string is safe to put in the MSI.
 */
export function notInCp1252(text) {
  const found = []
  const lines = text.split('\n')
  lines.forEach((line, i) => {
    for (const ch of line) {
      if (isCp1252(ch)) continue
      found.push({
        char: ch,
        codepoint: `U+${ch.codePointAt(0).toString(16).toUpperCase().padStart(4, '0')}`,
        line: i + 1,
        context: line.trim().slice(0, 80),
      })
    }
  })
  return found
}
