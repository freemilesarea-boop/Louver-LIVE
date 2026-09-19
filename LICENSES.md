# Licenses

## Louver Live

Proprietary. Copyright © Louver Live.

## FFmpeg

Louver Live bundles `ffmpeg` and `ffprobe` as Tauri sidecars. They are separate
programs, executed as child processes; no FFmpeg code is linked into the
application.

`scripts/fetch-ffmpeg.mjs` downloads them per platform:

| Platform | Source | License |
| --- | --- | --- |
| Windows x64 | [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) release-essentials | GPL v3 |
| macOS Intel | [evermeet.cx](https://evermeet.cx/ffmpeg/) | GPL v3 |
| macOS Apple Silicon | [osxexperts.net](https://www.osxexperts.net/) | GPL v3 |
| Linux x64 / arm64 | [johnvansickle.com](https://johnvansickle.com/ffmpeg/) static | GPL v3 |

The exact URL used for a build is recorded in
`apps/desktop/src-tauri/binaries/SOURCE-<target-triple>.txt`.

### Obligations

These builds are **GPL v3**. Distributing them alongside a proprietary
application is permissible because they are unmodified, separately-licensed
programs invoked as subprocesses rather than linked libraries — the same basis
on which other applications ship FFmpeg. Distribution nonetheless requires:

1. Shipping this notice with the application.
2. Making the corresponding FFmpeg source available. The builds above are
   unmodified upstream releases; linking to the corresponding release tag on
   <https://git.ffmpeg.org/ffmpeg.git> satisfies this.
3. Not removing FFmpeg's own copyright notices.

> **Before shipping commercially, have this reviewed by counsel.** If the
> conclusion is that GPL v3 is unacceptable, the fix is to build FFmpeg from
> source with LGPL-compatible options only (`--disable-gpl --disable-nonfree`,
> without `libx264`) and use the platform hardware encoders plus
> `mpeg4`/`openh264` instead. The `SOURCE-*.txt` files and this table are the
> places to update.

### Development builds

Running `scripts/fetch-ffmpeg.mjs` on a machine that cannot reach those hosts
falls back to copying the system's `ffmpeg`/`ffprobe`. That fallback is for
development only — its `SOURCE-*.txt` says so explicitly. Release builds must
use `--require-download`, which fails rather than falling back.

## Third-party Rust crates

Licences are MIT or Apache-2.0 unless noted. Generate the full manifest with:

```bash
cargo install cargo-about && cargo about generate about.hbs
```

Principal dependencies: `tauri` (MIT/Apache-2.0), `rusqlite` + bundled SQLite
(MIT / public domain), `serde`, `chrono`, `sysinfo`, `rand`, `sha2`, `hex`,
`base64`, `uuid`, `thiserror` (MIT/Apache-2.0), `ed25519-dalek`
(BSD-3-Clause), `keyring` (MIT/Apache-2.0).

## Third-party JavaScript packages

All MIT: `react`, `react-dom`, `zustand`, `lucide-react`, `@tauri-apps/api`,
`tailwindcss`, `vite`, `vitest`, `typescript`, `eslint`.

Run `npx license-checker --summary` for the full list.

## Test fixtures

No copyrighted media is committed to this repository. Every test video is
generated at test time by FFmpeg's `testsrc2`, `smptebars`, `color` and `sine`
sources.
