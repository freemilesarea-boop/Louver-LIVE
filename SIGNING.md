# Signing keys

Two separate keys, both of them yours to generate, neither of them ever in this
repository.

| Key | Signs | Where it lives | Where its public half goes |
| --- | --- | --- | --- |
| Apple Developer ID certificate | the `.app` and `.dmg` | macOS Keychain, exported to CI as a secret | Apple's notary service |
| Tauri updater key | update manifests | offline, exported to CI as a secret | `tauri.conf.json` → `plugin.updater.pubkey` |

There used to be a third: an Ed25519 key that signed `license.json` files
issued to customers, whose public half was compiled into the build. Louver Live
no longer verifies a licence — who may use it is decided before the installer
changes hands — so that key and everything around it are gone. Nothing in this
document is about permission to run the program; it is all about proving to an
operating system that the installer is what it says it is.

**No production private key is created by this repository and none may be
committed to it.** `.gitignore` already refuses `*.pem` and `private_key*`,
and `npm run secret-scan` fails the build if a key
reaches the working tree or the history. Generate them yourself, on your own
machine, following the steps below.

---

## 1. Apple Developer ID (macOS)

Needed so customers do not meet Gatekeeper. Requires a paid Apple Developer
account.

1. In Xcode or the developer portal, create a **Developer ID Application**
   certificate and install it in your login Keychain.
2. Export it as a `.p12` with a password.
3. Convert it for CI: `base64 -i cert.p12 | pbcopy`.
4. Create an app-specific password for notarization at appleid.apple.com.
5. Store these as repository secrets:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | the base64 `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | your Apple ID email |
| `APPLE_PASSWORD` | the app-specific password |
| `APPLE_TEAM_ID` | your team ID |

`.github/workflows/release.yml` passes these to `tauri-action`, which signs and
notarizes when they are present and builds unsigned when they are not. An
unsigned build is fine for testing and must be recorded as unsigned — see
`MACOS_RELEASE_TEST.md` step 4.

Verify the result on the built app:

```bash
spctl -a -vvv "Louver Live.app"     # expect: accepted, Notarized Developer ID
```

---

## 2. Windows code signing

Optional but strongly advised: without it, SmartScreen warns every customer on
first run.

An OV or EV certificate from a commercial CA, exported as `.pfx`, stored as
`WINDOWS_CERTIFICATE` (base64) and `WINDOWS_CERTIFICATE_PASSWORD`. EV
certificates usually live on a hardware token, which a hosted runner cannot
use — those have to be signed on a machine that holds the token.

---

## 3. Updater key

Only needed if the in-app updater is switched on.

```bash
npm run tauri signer generate -- -w ~/louver-keys/updater.key
```

The public half goes into `tauri.conf.json` under the updater plugin; the
private half becomes `TAURI_SIGNING_PRIVATE_KEY`, with its password in
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Same rule as the others: it never enters
this repository.

---

## Checklist before a production release

- [ ] Apple certificate and notarization secrets configured
- [ ] `spctl` reports the built app as accepted and notarized
- [ ] `npm run secret-scan` passes on the working tree and the history
- [ ] No `.pem`, `private_key*` or `*signing-key*` file anywhere in the repository
