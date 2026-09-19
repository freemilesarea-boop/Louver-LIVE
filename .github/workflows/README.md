# CI

`ci.yml` runs `npm run verify` on Linux, Windows and macOS for every push. That
is the same command a developer runs locally, so there is one definition of
"passing" rather than two.

`bundle.yml` (the `bundle` job in `ci.yml`) only runs on `main` and on version
tags. It uses `--require-download`, so a release can never accidentally ship the
development FFmpeg fallback.

`soak.yml` is manual. A 24-hour test does not belong in CI; this exists so a
release candidate can be soaked on demand and the results kept as an artifact.

## Secrets

| Secret | Used for |
| --- | --- |
| `LOUVER_LICENSE_PUBLIC_KEY` | Embedded at build time so issued licences verify (§46) |
| `TAURI_SIGNING_PRIVATE_KEY` | Signs update bundles (§48) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Passphrase for the above |

The licence **private** key is never in CI, never in this repository, and never
in `.env`. It lives offline and is used only by `tools/license-generator`.
