# macOS release test — ordered runbook

Eighteen steps, in order, to be run on a real Apple Silicon Mac. Each step says
what to do, what counts as a pass, and **where to write the result**. Nothing
here may be marked PASS from reading the code; a step that was not run is
**NOT TESTED**.

`MACOS_QA.md` is the wider checklist of everything worth exercising. This file
is the shorter, strictly ordered path that decides whether the build ships.

## Before you start

| | |
| --- | --- |
| Machine | Apple Silicon (M1 or later), macOS 11 or later |
| Disk | 20 GB free — normalized video is roughly 4.6 GB per hour at 1080p30 |
| Needed | Two or more MP4 music videos, and a YouTube Live stream key |
| Time | About 90 minutes, plus the length of the broadcast test |

**The stream key is typed into the app's own settings screen and nowhere
else.** Not into a terminal, not into a file, not into a chat window. If a key
has ever been pasted somewhere else, rotate it in YouTube Studio first.

Record results in **`rc-results/macos-release-test.md`** — copy the results
table at the bottom of this file into it and fill it in as you go. Anything
that fails also gets an entry in `RELEASE_CANDIDATE_REPORT.md` §16 (Release
blockers).

---

## 1. Get the build

Download the `louver-live-aarch64-apple-darwin` artifact from the
**Release artifacts** workflow run, or build locally:

```bash
npm ci
node scripts/fetch-ffmpeg.mjs --require-download --target aarch64-apple-darwin
node scripts/ffmpeg-manifest.mjs --check
npm run build
```

**Pass:** a `.dmg` and a `.app` exist under
`apps/desktop/src-tauri/target/aarch64-apple-darwin/release/bundle/`.
**Record:** artifact filename and SHA — `shasum -a 256 <file>`.

## 2. Confirm the sidecars are the shipped ones

```bash
node scripts/ffmpeg-manifest.mjs --check
```

**Pass:** exit 0, every binary reported static, licence identified, `-fps_mode`
supported, no `DEVELOPMENT ONLY` marker.
**Record:** paste the manifest summary. A non-zero exit is a **release
blocker** — stop here.

## 3. Install from the .dmg

Open the `.dmg`, drag the app to Applications, eject, and launch from
Applications — not from the mounted image.

**Pass:** the app opens. **Record:** whether Gatekeeper appeared, and its exact
wording.

## 4. Gatekeeper and signature

```bash
codesign -dv --verbose=4 /Applications/Louver\ Live.app 2>&1 | head -20
spctl -a -vvv /Applications/Louver\ Live.app
```

**Pass (signed release):** `spctl` says `accepted`, source `Notarized
Developer ID`.
**Pass (unsigned test build):** `spctl` rejects it and the app opens only via
right-click → Open. That is expected for an unsigned build and must be recorded
as **unsigned**, never as a pass of the signing requirement.
**Record:** both command outputs verbatim.

## 5. First-run wizard

**Pass:** the wizard appears, explains what the app does, and finishes without
error. **Record:** any step that is confusing or wrong.

## 6. Enter the stream key

Settings → type the key into the field → save.

**Pass:** the field shows a mask, and an obvious hint (last four characters) —
never the whole key.
**Record:** confirm the masking, and note the backend shown ("macOS 키체인").

## 7. Prove the key is in the Keychain and not in the database

```bash
sqlite3 ~/Library/Application\ Support/com.louver.live/louver.db \
  "select * from settings;" | grep -i -c "<first 4 chars of the key>"
security find-generic-password -s "Louver Live" -a "stream_key" -w | head -c 4
```

**Pass:** the first command prints `0`. The second prints the first four
characters, proving the Keychain holds it.
**Record:** the count (must be 0) — do not paste the key itself.

## 8. Import two videos

Add two MP4s of different resolutions or frame rates.

**Pass:** both import; optimization runs with visible progress; both end as
"최적화 완료". **Record:** wall time per video and its length, so
normalization throughput can be compared against BENCHMARK.md §3.

## 9. Check what optimization produced

```bash
ffprobe -v error -select_streams v:0 \
  -show_entries stream=codec_name,width,height,r_frame_rate,pix_fmt \
  -of default=nw=1 ~/Library/Application\ Support/com.louver.live/cache/<file>
```

**Pass:** identical parameters for both files — h264, 1920x1080, 30/1,
yuv420p. Differing parameters break concat stream copy and are a **blocker**.
**Record:** both outputs.

## 10. Preflight

Build a playlist from both videos and press the broadcast button.

**Pass:** the preflight panel runs its checks and the FFmpeg row passes.
**Record:** any warning text shown.

## 11. Go live (30 minutes)

Start the broadcast against the real YouTube key. Watch the YouTube Studio
dashboard.

**Pass:** YouTube shows the stream as healthy; the app shows LIVE; the
broadcast runs 30 minutes without interruption.
**Record:** start time, end time, YouTube's health indicator, and any warning
YouTube raised.

## 12. Prove it is still stream copy

While the broadcast is live:

```bash
ps -Ao args | grep '[f]fmpeg' | tr ' ' '\n' | grep -E 'libx264|h264_|-c:v'
```

**Pass:** no output. The live command must contain `-c copy` and no encoder.
Settings → Developer Mode → Streaming Mode shows the same thing in the UI.
**Record:** the command output (empty) and the UI's reported mode.

## 13. CPU and memory over the broadcast

```bash
while true; do
  ps -Ao pid,pcpu,rss,comm | grep -E 'ffmpeg|Louver' | grep -v grep
  echo "---"; sleep 300
done | tee ~/louver-macos-cpu.log
```

**Pass:** FFmpeg CPU stays in low single digits, and neither process grows
steadily in RSS.
**Record:** first and last readings, and the peak — into the results table and
into BENCHMARK.md if this is the first macOS measurement.

## 14. Playlist boundary

Watch the YouTube player across at least two video transitions.

**Pass:** no freeze, no black frame, no silence, no buffering spinner, no
"reconnecting" in the app.
**Record:** how many transitions were watched and what was seen.

## 15. Network interruption

Turn Wi-Fi off for 60 seconds, then back on.

**Pass:** the app goes RECONNECTING, retries with a widening gap, returns to
LIVE by itself, and the app never crashes. YouTube may end the broadcast on its
side — note whether it did.
**Record:** seconds to notice, seconds to recover, number of attempts, and what
YouTube did.

## 16. Kill FFmpeg underneath it

```bash
pkill -f 'ffmpeg.*rtmps'
```

**Pass:** the app notices, restarts FFmpeg, and returns to LIVE on its own.
**Record:** recovery time, and confirm no orphan FFmpeg remains
(`pgrep -fl ffmpeg`).

## 17. Sleep prevention

Leave the broadcast running and do not touch the machine for 20 minutes with
the display sleep set to 5 minutes.

```bash
pmset -g assertions | grep -i -E 'PreventUserIdleSystemSleep|Louver'
```

**Pass:** the assertion is held while broadcasting, the machine does not sleep,
and the broadcast is unbroken. The assertion must be released after Stop.
**Record:** the assertion line, and whether it disappeared after stopping.

## 18. Stop, then audit what was left behind

Press Stop. Then:

```bash
pgrep -fl ffmpeg
grep -ric "<first 4 chars of the key>" ~/Library/Logs/com.louver.live/ \
  ~/Library/Application\ Support/com.louver.live/ | grep -v ':0$'
```

**Pass:** the broadcast ends cleanly, no FFmpeg process survives, and the
second command prints nothing — the key appears in no log and no file.
**Record:** both outputs. A key found anywhere is a **release blocker**.

---

## Results table

Copy this into `rc-results/macos-release-test.md` and fill it in. Leave a row
as NOT TESTED rather than guessing.

```markdown
# macOS release test — <date>

Machine: <model, chip, macOS version>
Build:   <artifact name> <sha256>
Tester:  <name>

| # | Step | Result | Measured / notes |
| --- | --- | --- | --- |
| 1 | Get the build | | |
| 2 | Sidecar manifest | | |
| 3 | Install from .dmg | | |
| 4 | Gatekeeper and signature | | |
| 5 | First-run wizard | | |
| 6 | Enter the stream key | | |
| 7 | Key in Keychain, not in the database | | |
| 8 | Import two videos | | |
| 9 | Optimization output parameters | | |
| 10 | Preflight | | |
| 11 | 30-minute YouTube broadcast | | |
| 12 | Still stream copy | | |
| 13 | CPU and memory | | |
| 14 | Playlist boundary | | |
| 15 | Network interruption | | |
| 16 | FFmpeg killed | | |
| 17 | Sleep prevention | | |
| 18 | Stop and audit | | |
```

## What a pass means

The build ships only if steps 1–18 are all PASS, with two allowances:

- Step 4 may record **unsigned** for a test build. It must be PASS, signed and
  notarized, before the build goes to a customer.
- Step 15 may record that YouTube ended the broadcast on its side. That is
  YouTube's behaviour, not a defect, as long as the app recovered its own
  connection and did not crash.

Any other FAIL is a release blocker and goes into
`RELEASE_CANDIDATE_REPORT.md` §16.
