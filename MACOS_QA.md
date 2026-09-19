# macOS QA checklist

**Status: NOT TESTED.** This release candidate was developed and verified on
Linux. Nothing in this document has been executed on macOS, and no macOS claim
appears anywhere in the release report.

Everything here compiles for macOS and is covered by tests that run the same
code paths through platform-independent traits, but the macOS implementations
themselves — Keychain, `caffeinate`, the launch agent, the tray — have never
run. They must be executed on real hardware before release.

Run this on both an Apple Silicon and, if supported, an Intel Mac.

**For the decision about whether a build ships, use
`MACOS_RELEASE_TEST.md`** — eighteen ordered steps with a results table. This
file is the wider checklist to work through once that path passes.

**Getting a build without a Mac:** the `Release artifacts` workflow
(`.github/workflows/release.yml`) builds the `.app` and `.dmg` on a `macos-14`
Apple Silicon runner. The build path is **BUILD READY**; nothing has been run
on macOS hardware, so everything below stays **NOT TESTED**.

## Build

```bash
npm ci
node scripts/fetch-ffmpeg.mjs --require-download --force
node scripts/ffmpeg-manifest.mjs --check     # must pass before bundling
LOUVER_LICENSE_PUBLIC_KEY=<production public key> npx tauri build
```

- [ ] `.dmg` and `.app` produced
- [ ] `node scripts/ffmpeg-manifest.mjs --check` passes (static, licence recorded)
- [ ] `npm run verify` passes on macOS
- [ ] App is code-signed and notarized; a clean Mac opens it without a Gatekeeper warning

## Install and first run

- [ ] Drag-install from the `.dmg` works
- [ ] First launch shows the 5-step wizard in Korean, not mojibake
- [ ] Skipping the wizard still reaches the dashboard
- [ ] `~/Library/Application Support/LouverLive/` is created with `louver.db`, `cache/`, `logs/`
- [ ] `logs/app.log` records the app version and the chosen encoder
- [ ] The chosen encoder is `h264_videotoolbox`, **not** `libx264` — if it is libx264,
      hardware detection failed and normalization will be several times slower

## Keychain (§15)

This is the highest-risk untested area: the Linux fallback is memory-only, so
the Keychain path has never executed.

- [ ] Saving a stream key succeeds without an unexpected permission prompt loop
- [ ] Settings shows `macOS 키체인` as the backend and reports it as secure
- [ ] The key survives an app restart
- [ ] `security find-generic-password -s com.louver.live` shows the item exists
- [ ] Deleting the key in Settings removes it from the Keychain
- [ ] `node scripts/secret-scan.mjs --runtime` finds nothing in the app data directory
- [ ] `grep -r <the key> ~/Library/Application\ Support/LouverLive/` returns nothing

## Media

- [ ] Drag-and-drop import works from Finder
- [ ] A file at `/Users/<you>/Music/오늘 밤 재즈.mp4` (Korean, spaces) imports, optimizes and broadcasts
- [ ] A path containing an apostrophe (`rock'n'roll.mp4`) works
- [ ] Optimization shows progress and the remaining file count
- [ ] Cancelling optimization stops promptly and leaves no partial file in `cache/`
- [ ] A file already optimized shows `송출 준비 완료` and is **not** re-encoded on a second run
- [ ] Replacing a source file on disk makes it require optimization again
- [ ] The disk estimate appears before optimization and refuses when space is short

## Playlist and schedule

- [ ] Drag-and-drop reordering persists across a restart
- [ ] Disabling an item excludes it from the broadcast
- [ ] Total duration matches the sum of enabled items
- [ ] A `20:00 → 08:00` schedule shows `자정 넘김`
- [ ] The schedule starts the broadcast at the stated minute, unattended

## Broadcasting (needs a stream key)

- [ ] `로컬 테스트` produces a playable file and shows `TEST`, not `LIVE`
- [ ] A real broadcast appears on YouTube within ~30s
- [ ] Developer mode → Streaming Mode reports `STREAM COPY`
- [ ] FFmpeg CPU stays low (single digits); if it is high, check the mode first
- [ ] Turning Wi-Fi off for 60s moves the app to `재연결 중` and it recovers when Wi-Fi returns
- [ ] Quitting FFmpeg from Activity Monitor triggers automatic recovery
- [ ] An explicit Stop does **not** auto-restart

## System integration

- [ ] Tray icon appears in the menu bar with all four items
- [ ] Closing the window while broadcasting hides it and keeps broadcasting
- [ ] Tray → `프로그램 종료` stops the broadcast and leaves no `ffmpeg` in Activity Monitor
- [ ] `컴퓨터가 켜지면 자동 실행` creates a login item (System Settings → General → Login Items)
- [ ] Sleep prevention: during a broadcast `pmset -g assertions` shows an assertion held
- [ ] Sleep prevention is released after the broadcast ends
- [ ] Closing the laptop lid stops the broadcast — this is expected and the UI says so

## Restart recovery (§10)

- [ ] Set a schedule covering the current time, enable autostart, and reboot
- [ ] After login the app starts by itself
- [ ] The broadcast resumes without any interaction
- [ ] `logs/app.log` shows the recovery notice
- [ ] Force-quitting the app mid-broadcast and reopening it resumes (inside the window)
- [ ] Doing the same outside the window does **not** start a broadcast
- [ ] No orphaned `ffmpeg` process remains in either case

## Long run

- [ ] `npm run soak -- --duration 6h` completes with no restarts and flat memory
- [ ] A 24-hour broadcast to a private YouTube stream survives intact
