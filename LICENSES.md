# Licenses

## Louver Live

Proprietary. Copyright © Louver Live.

---

## FFmpeg — distribution decision (§15)

Louver Live bundles `ffmpeg` and `ffprobe` as Tauri sidecars. They are separate
programs, executed as child processes with an argv vector; no FFmpeg code is
linked into the application.

**Every shipped binary must be recorded here and verified by**
`node scripts/ffmpeg-manifest.mjs --check`, which reads the version, provider,
licence, linkage, encoders and protocol support out of the binary itself and
fails the build if anything is unfit. A binary whose licence is not certain is
not shipped.

### What the product actually needs

This drives the whole decision, so it is worth stating precisely:

| Path | When | Needs an H.264 **encoder**? |
| --- | --- | --- |
| Live broadcast | every second the app is on air | **No** — it is a remux (`-c copy`) |
| Normalization | once per video, at import | Yes |
| Compatibility mode | only when stream copy is unsuitable | Yes |

Because the live path never encodes, the encoder requirement applies only to
import-time optimization — which every supported platform can satisfy with an
OS or hardware encoder.

### Minimum version: FFmpeg 5.1

Licence is not the only thing that disqualifies a build. Normalization uses
`-fps_mode`, which replaced `-vsync` in **FFmpeg 5.1**; an older binary rejects
it and every import fails. This was found the hard way — both FFmpeg builds
distributed through npm (`@ffmpeg-installer/darwin-arm64` 4.1.5,
`@ffmpeg-installer/win32-x64` 4.1.0) are 4.1 and cannot optimize a single
video, while broadcasting fine, because broadcasting is a remux.

Two things enforce it:

- `scripts/ffmpeg-manifest.mjs --check` probes `-fps_mode` on the actual
  binary and marks anything below 5.1 UNFIT, so a too-old build cannot reach a
  release.
- At runtime `FfmpegTools::capabilities()` asks the binary what it supports —
  RTMPS, an H.264 encoder **proved by encoding a frame**, AAC, `-fps_mode` —
  rather than parsing its version string. Preflight then separates the two
  answers: no RTMPS blocks broadcasting outright; no encoder or no `-fps_mode`
  is a warning, because an already-optimized library still broadcasts.

The encoder fallback chain is checked the same way: each candidate is proved on
the real binary before it is used, so the chain can never select an encoder
that the shipped build does not actually have. That is what makes Option B
below safe to switch to without touching code.

### Option A — GPL build (default, chosen for v1.0)

| | |
| --- | --- |
| Licence | **GPL v3** (or GPL v2-or-later, depending on build flags) |
| Encoders | `libx264` plus whatever hardware the machine has |
| Linkage | static |
| Why | libx264 is the highest-quality software H.264 encoder and works everywhere, including machines with no usable hardware encoder |

Sources `scripts/fetch-ffmpeg.mjs` downloads:

| Platform | Provider | Licence |
| --- | --- | --- |
| Windows x64 | [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) `release-essentials` | GPL v3 |
| macOS Intel | [evermeet.cx](https://evermeet.cx/ffmpeg/) | GPL v3 |
| macOS Apple Silicon | [osxexperts.net](https://www.osxexperts.net/) | GPL v3 |
| Linux x64 / arm64 | [johnvansickle.com](https://johnvansickle.com/ffmpeg/) static | GPL v3 |

Both macOS providers publish **one download per tool**, so `ffprobe` is fetched
from its own archive there; gyan.dev and johnvansickle ship both in one. The
URLs a build actually used are written to `binaries/SOURCE-<triple>.txt` and
`ffmpeg-manifest.mjs --check` refuses to release a binary whose source is not
recorded.

**Obligations when shipping a GPL build:**

1. Ship this notice with the application.
2. Make the corresponding FFmpeg source available. These are unmodified
   upstream releases, so linking to the matching release tag at
   <https://git.ffmpeg.org/ffmpeg.git> satisfies it.
3. Do not remove FFmpeg's own copyright notices.
4. Be aware that GPL v3 is widely read as incompatible with the Apple App
   Store's terms. Direct `.dmg` distribution is unaffected; App Store
   distribution would require Option B.

### Option B — LGPL build (available, not chosen)

| | |
| --- | --- |
| Licence | LGPL v2.1-or-later |
| Build flags | `--disable-gpl --disable-nonfree`, **without** libx264/libx265 |
| Encoders | `h264_videotoolbox` (macOS), `h264_mf` / `h264_nvenc` / `h264_qsv` / `h264_amf` (Windows), `libopenh264` as the portable software fallback |
| Trade-off | Slightly lower quality-per-bit at the same bitrate, and normalization quality then varies with the user's hardware |

This option is genuinely viable here because the live path needs no encoder at
all. The code supports it: the encoder chain prefers hardware, then falls back
through `libx264` -> `libopenh264` -> `h264_mf`, and each candidate is proved by
encoding a frame before it is used. An LGPL build simply never reaches
`libx264`.

`libopenh264` is BSD-licensed; Cisco publishes a binary release that covers the
H.264 patent royalties for redistributors who ship that binary unmodified.
Confirm that arrangement applies before relying on it.

### Decision

**v1.0 ships Option A (GPL v3).** Direct download distribution only; not the
Mac App Store. This is recorded as a release blocker requiring sign-off by
someone qualified — see RELEASE_CANDIDATE_REPORT.md. If the answer is that
GPL v3 is unacceptable, switch to Option B, rebuild the sidecars, re-run
`scripts/ffmpeg-manifest.mjs --check`, and re-measure normalization throughput
in BENCHMARK.md, since the encoder changes.

### The development fallback is never shipped

If `fetch-ffmpeg.mjs` cannot reach the download hosts it copies the machine's
own `ffmpeg`/`ffprobe` so development and tests can proceed, and writes
`DEVELOPMENT ONLY` into the accompanying `SOURCE-*.txt`. Release builds use
`--require-download`, which fails rather than falling back, and
`ffmpeg-manifest.mjs --check` rejects any fallback binary it finds.

The binary used while developing this release candidate was, for the record:

```
ffmpeg 6.1.1-3ubuntu5   (Ubuntu system package)
licence   GPL-2.0-or-later  (--enable-gpl, libx264, libx265)
linkage   DYNAMIC — 215 shared libraries
verdict   UNFIT TO SHIP (dynamically linked; would not run on a user's machine)
```

---

## Third-party Rust crates

MIT or Apache-2.0 unless noted. Generate the full manifest with:

```bash
cargo install cargo-about && cargo about generate about.hbs
```

Principal dependencies: `tauri` (MIT/Apache-2.0), `rusqlite` with bundled
SQLite (MIT / public domain), `serde`, `chrono`, `sysinfo`, `rand`, `sha2`,
`hex`, `base64`, `uuid`, `thiserror` (MIT/Apache-2.0), `ed25519-dalek`
(BSD-3-Clause), `keyring` (MIT/Apache-2.0).

## Third-party JavaScript packages

All MIT: `react`, `react-dom`, `zustand`, `lucide-react`, `@tauri-apps/api`,
`tailwindcss`, `vite`, `vitest`, `typescript`, `eslint`.

Run `npx license-checker --summary` for the full list. Note that the
development toolchain (Vite, Vitest, esbuild) is not shipped.

## Test fixtures

No copyrighted media is committed. Every test video is generated at test time
from FFmpeg's `testsrc2`, `smptebars`, `color` and `sine` sources.
