# Signing and licence keys

Three separate keys, all of them yours to generate, none of them ever in this
repository.

| Key | Signs | Where it lives | Where its public half goes |
| --- | --- | --- | --- |
| Licence signing key (Ed25519) | licence files you issue to customers | offline, on your machine | compiled into the build via `LOUVER_LICENSE_PUBLIC_KEY` |
| Apple Developer ID certificate | the `.app` and `.dmg` | macOS Keychain, exported to CI as a secret | Apple's notary service |
| Tauri updater key | update manifests | offline, exported to CI as a secret | `tauri.conf.json` → `plugin.updater.pubkey` |

**No production private key is created by this repository and none may be
committed to it.** `.gitignore` already refuses `*.pem`, `private_key*` and
`license-signing-key*`, and `npm run secret-scan` fails the build if a key
reaches the working tree or the history. Generate them yourself, on your own
machine, following the steps below.

---

## 1. Licence signing key

### Generate it (once, offline)

```bash
cargo run -p license-generator -- keygen --out ~/louver-keys
```

It writes the private key to `~/louver-keys/license-signing-key.txt` (mode
0600) and **prints the public key to the terminal** — copy that line somewhere
you can find it again:

```
private key -> /Users/you/louver-keys/license-signing-key.txt  (KEEP OFFLINE, NEVER COMMIT)
public key  -> Vfz4oXmbtD9XYji2rkS7VGGxpvjWHOwf5eCU2qXj82s=
```

The private key **never leaves your machine.** Back it up offline — a password
manager or an encrypted USB stick. Lose it and you cannot issue licences to
existing customers under the same public key; leak it and anyone can mint
licences.

Keep the directory out of any synced folder — not Dropbox, not iCloud Drive,
not a git repository.

### Put the public key into the build

The public key is read at compile time. The placeholder compiled into a
development build is all zeroes and verifies nothing, which is the safe
default — a build that forgot the real key rejects every licence rather than
accepting every licence.

```bash
export LOUVER_LICENSE_PUBLIC_KEY="<the public key printed by keygen>"
npm run build
```

In CI, store it as the repository secret `LOUVER_LICENSE_PUBLIC_KEY` — it is a
public key, so this is not a secret in the cryptographic sense, but keeping it
with the others keeps the release reproducible.

### Issue a licence

```bash
cargo run -p license-generator -- issue \
  --key ~/louver-keys/license-signing-key.txt \
  --id LL-2026-0001 \
  --edition Lifetime \
  --out ~/licenses/LL-2026-0001.json
```

Send the customer the JSON file. They install it through Settings → Licence.

To bind a licence to a machine, add `--device <fingerprint>`; the customer
finds their fingerprint in Settings → Licence. Binding is only enforced when
`enforce_device_binding` is on.

### Prove it before shipping

Four cases, all four of which must behave as stated. Run them against a
**release** build, because the development escape hatch follows
`debug_assertions` and is compiled out of release builds.

Two commands, and the difference between them matters:

- `verify --pub <key>` checks a licence against a key you name. Use it to
  check a licence before sending it to a customer.
- `verify-build` checks against the key **compiled into this build** — which is
  what the app itself will use. This is the one that catches a build that
  still carries the placeholder key.

| # | Case | How | Expected |
| --- | --- | --- | --- |
| 1 | A valid licence | issue one, then `verify --pub` | `VALID … perpetual` — and in the app, Settings shows the edition and ID |
| 2 | A tampered licence | edit any field in the JSON by hand | `INVALID signature does not match`; in the app, `LL-LICENSE-002`. The signature covers a canonical serialization, so one changed character invalidates it |
| 3 | An expired licence | `issue --expires 2020-01-01T00:00:00Z` | `EXPIRED … signature is good, but it expired`; in the app, `LL-LICENSE-004` |
| 4 | A licence from a different key | `keygen` into a throwaway directory, issue with that key, check with your real public key | `INVALID signature does not match`; in the app, `LL-LICENSE-002` |

All four were run against this build and behave exactly as stated above. What
they prove is that the *verification rules* are right.

```bash
cargo run -p license-generator -- verify-build ~/licenses/LL-2026-0001.json
```

What is **not** yet proved is that a shipped build carries your key, because no
production key exists yet — this repository must never create one. Today
`verify-build` correctly answers:

```
error: this binary was built with the placeholder public key;
       rebuild with LOUVER_LICENSE_PUBLIC_KEY=<your public key>
```

That refusal is the safe behaviour, and it is also a **release blocker** until
you generate your key, rebuild with it, and see `verify-build` accept a real
licence while rejecting a case-4 licence.

The same four cases are asserted automatically in
`crates/louver-core/tests/rc_license.rs` against a generated test keypair. That
proves the verification rules; only the manual run above proves the *shipped
build* carries your key.

---

## 2. Apple Developer ID (macOS)

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

## 3. Windows code signing

Optional but strongly advised: without it, SmartScreen warns every customer on
first run.

An OV or EV certificate from a commercial CA, exported as `.pfx`, stored as
`WINDOWS_CERTIFICATE` (base64) and `WINDOWS_CERTIFICATE_PASSWORD`. EV
certificates usually live on a hardware token, which a hosted runner cannot
use — those have to be signed on a machine that holds the token.

---

## 4. Updater key

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

- [ ] Licence signing key generated offline and backed up
- [ ] `LOUVER_LICENSE_PUBLIC_KEY` set for the release build
- [ ] All four licence cases verified against the **release** build
- [ ] `cargo run -p license-generator -- verify-build` accepts a real licence
- [ ] Apple certificate and notarization secrets configured
- [ ] `spctl` reports the built app as accepted and notarized
- [ ] `npm run secret-scan` passes on the working tree and the history
- [ ] No `.pem`, `private_key*` or `*signing-key*` file anywhere in the repository
