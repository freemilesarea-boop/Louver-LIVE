# Windows QA checklist

**Status: NOT TESTED.** This release candidate was developed and verified on
Linux. No Windows machine or VM was available, so nothing below has been
executed and no Windows claim appears in the release report.

Target: Windows 10 and 11, x64. Run on both if possible, and on at least one
machine with no NVIDIA GPU so the encoder fallback is exercised.

## Build

```powershell
npm ci
node scripts/fetch-ffmpeg.mjs --require-download --force
node scripts/ffmpeg-manifest.mjs --check     # must pass before bundling
$env:LOUVER_LICENSE_PUBLIC_KEY="<production public key>"; npx tauri build
```

- [ ] `.msi` and NSIS `.exe` produced
- [ ] `node scripts/ffmpeg-manifest.mjs --check` passes
- [ ] `npm run verify` passes on Windows — especially the path tests, which are
      the ones most likely to behave differently here
- [ ] The installer is signed; SmartScreen does not warn on a clean machine

## Install and first run

- [ ] The `.msi` installs per-machine without errors
- [ ] Launching from the Start menu works
- [ ] First launch shows the wizard in Korean, correctly rendered
- [ ] `%APPDATA%\LouverLive\` is created with `louver.db`, `cache\`, `logs\`
- [ ] No console window appears alongside the GUI
- [ ] `logs\app.log` records the chosen encoder

## FFmpeg sidecar

- [ ] `ffmpeg.exe` and `ffprobe.exe` are installed next to the app binary
- [ ] Settings → Advanced shows the FFmpeg version, not `LL-CONFIG-002`
- [ ] The chosen encoder is a hardware one (`h264_nvenc` / `h264_qsv` / `h264_amf`)
      on a machine that has one
- [ ] On a machine **without** a supported GPU it falls back to `libx264` —
      it must not pick `h264_nvenc`, which is listed even where no NVIDIA card exists
- [ ] Windows Defender does not quarantine the sidecar

## Credential Manager (§15)

Never executed; the Linux fallback is memory-only.

- [ ] Saving a stream key succeeds
- [ ] Settings shows `Windows 자격 증명 관리자` and reports it as secure
- [ ] The key survives an app restart and a reboot
- [ ] It appears under Control Panel → Credential Manager → Windows Credentials as `com.louver.live`
- [ ] Deleting it in Settings removes the credential
- [ ] `node scripts/secret-scan.mjs --runtime` finds nothing under `%APPDATA%\LouverLive`

## Paths — the highest-risk Windows area

- [ ] `C:\Users\Test User\Music\재즈 영상 01.mp4` (spaces **and** Korean) imports, optimizes, broadcasts
- [ ] A path with an apostrophe works
- [ ] A path longer than 260 characters is either handled or refused with a clear message, not a crash
- [ ] A file on a mapped network drive works, or fails with a clear message
- [ ] A file on a removable drive that is then unplugged produces `LL-MEDIA-003`, not a crash
- [ ] The cache directory under `%APPDATA%` handles a Korean Windows username

## Playlist and schedule

- [ ] Drag-and-drop reordering persists across a restart
- [ ] Total duration is correct
- [ ] A `20:00 → 08:00` schedule shows `자정 넘김` and fires at both ends
- [ ] The schedule starts the broadcast unattended at the stated minute

## Broadcasting (needs a stream key)

- [ ] `로컬 테스트` produces a playable file and shows `TEST`
- [ ] A real broadcast appears on YouTube within ~30s
- [ ] Developer mode → Streaming Mode reports `STREAM COPY`
- [ ] FFmpeg CPU stays low in Task Manager
- [ ] Disabling the network adapter for 60s moves the app to `재연결 중`; re-enabling recovers it
- [ ] Ending `ffmpeg.exe` from Task Manager triggers automatic recovery
- [ ] An explicit Stop does **not** auto-restart

## System integration

- [ ] Tray icon appears in the notification area with all four items
- [ ] Closing the window while broadcasting hides it and keeps broadcasting
- [ ] Tray → `프로그램 종료` stops the broadcast and leaves no `ffmpeg.exe` in Task Manager
- [ ] `컴퓨터가 켜지면 자동 실행` creates the Run entry and it survives a reboot
- [ ] Sleep prevention holds during a broadcast: `powercfg /requests` shows a SYSTEM request
- [ ] The request is released after the broadcast ends
- [ ] The machine does not sleep during a 30-minute unattended broadcast

## Restart recovery (§10)

- [ ] Set a schedule covering the current time, enable autostart, and reboot
- [ ] After login the app starts by itself
- [ ] The broadcast resumes without interaction
- [ ] Killing the app with Task Manager mid-broadcast and reopening resumes (inside the window)
- [ ] Doing the same outside the window does **not** start a broadcast
- [ ] No orphaned `ffmpeg.exe` remains in either case

## Long run

- [ ] `npm run soak -- --duration 6h` completes with no restarts and flat memory
- [ ] A 24-hour broadcast to a private YouTube stream survives intact
