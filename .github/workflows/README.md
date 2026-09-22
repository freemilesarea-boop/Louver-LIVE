# CI and releases

`ci.yml` runs `npm run verify` on Linux, Windows and macOS for every push. That
is the same command a developer runs locally, so there is one definition of
"passing" rather than two. Its `bundle` job is a build smoke test on `main` and
on tags: it proves the app still bundles, and its output is not what ships.

`release.yml` is what ships. It builds all four installers — Windows, macOS
Apple Silicon, macOS Intel, Linux — and on a `v*` tag publishes them as a
**draft** GitHub Release. `RELEASING.md` is the procedure; this file is only
about the secrets.

`soak.yml` is manual. A 24-hour test does not belong in CI; this exists so a
release candidate can be soaked on demand and the results kept as an artifact.

## Secrets

Every one of these is optional in the sense that the build succeeds without it.
None of them is optional in the sense that the result is the same —
`RELEASING.md` says what each one changes.

| Secret | Used for |
| --- | --- |
| `LOUVER_GOOGLE_CLIENT_ID` | The OAuth client baked into the release, so a user never opens the Google Cloud console |
| `LOUVER_GOOGLE_CLIENT_SECRET` | The same client's secret. Google refuses this desktop client's token exchange without it, so both are needed or neither works |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | Signing and notarizing the macOS builds |
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | Signing the Windows installer |
| `TAURI_SIGNING_PRIVATE_KEY` | Signs update bundles (§48) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Passphrase for the above |

There is no licence key here any more. Louver Live had an in-app licence gate
— a signed `license.json` had to be installed before a broadcast would start —
and it is gone. Who may use the program is decided before the installer is
handed over, so the app's job once it is running is to broadcast.

The Google client secret is a secret here and nowhere else: it is compiled into
the binary, which Google's own documentation allows for an installed app, and
it is never written to a log, the database or the UI.
