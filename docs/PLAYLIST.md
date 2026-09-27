# 247streams — 플레이리스트 방송이 어떻게 동작하는가

이 문서는 §1–§9의 구현 방식을 설명합니다. 코드를 읽기 전에 읽으면 왜 이렇게
만들었는지 알 수 있습니다.

---

## 1. 데이터 구조

```
Broadcast
 ├─ metadata        title, description, tags, category, privacy   (247streams 내부)
 ├─ playlist[]      broadcast_items: media_id, position, enabled, repeat_count
 ├─ destination     stream_destinations (kind = manual_rtmps | youtube_account)
 ├─ settings        resolution, fps, video/audio bitrate (기본 auto)
 ├─ schedule        start_at, stop_at, timezone, offset, repeat_days (UTC 저장)
 └─ runtime         desired_state, runtime_state, current_index, current_item,
                    next_item, position, cycle, play_count, restart_count, pid
```

`broadcasts.media_id` 컬럼은 **그대로 남아 있고 계속 갱신됩니다** — 첫 번째
항목을 가리킵니다. 이전 릴리스로 되돌려도 방송이 읽히고, 이전 릴리스가 만든
방송도 그대로 재생됩니다.

## 2. 연속 송출 (§3)

핵심: **영상이 바뀔 때 RTMP 연결이 끊기지 않습니다.**

FFmpeg 하나가 concat 매니페스트 하나를 입력으로 받습니다.

```
ffmpeg -re -stream_loop -1 -f concat -safe 0 -i manifest.txt \
       -c copy -f flv -flvflags no_duration_filesize rtmps://.../KEY
```

`manifest.txt`:

```
file '/…/prepared-clip1.mp4'
file '/…/prepared-clip2.mp4'
file '/…/prepared-clip3.mp4'
```

- **영상 전환** = 한 입력 안의 파일 경계입니다. 프로세스도, TCP 연결도, RTMP
  세션도 그대로입니다. YouTube는 끊김을 보지 못합니다.
- **전체 반복** = `-stream_loop -1`. 마지막 파일이 끝나면 첫 파일로 돌아갑니다.
- **반복 횟수** = 같은 파일을 매니페스트에 그만큼 씁니다.
- **사용 안 함** = 매니페스트에서 빠집니다(행은 남습니다).

실측(리눅스, 실제 RTMP 엔드포인트, 10초짜리 3개):

```
publisher connected     06:36:27      ← 단 한 번
  2s  1/3  now=clip1  next=clip2
 11s  2/3  now=clip2  next=clip3
 20s  3/3  now=clip3  next=clip1
 32s  1/3  now=clip1  next=clip2      ← 한 바퀴 돌고 다시 처음
publisher disconnected  06:37:02      ← 내가 STOP을 눌렀을 때
restart_count = 0 (전 구간)
```

`restart_count`가 0인 것이 증거입니다. 전환마다 연결이 끊겼다면 FFmpeg가 종료되고
supervisor가 재시작해 이 숫자가 올라갑니다.

### 왜 영상마다 FFmpeg를 새로 띄우지 않는가

띄우면 매번 RTMP 연결이 끊어집니다. YouTube는 몇 초의 공백을 스트림 종료로
간주할 수 있고, 최악의 경우 라이브 자체가 끝납니다. concat + stream copy는 이
문제를 아예 없앱니다.

### 왜 모든 업로드를 한 번 remux 하는가

concat으로 이어 붙이려면 각 파일이 **프레임 경계에서 정확히 끝나야** 합니다.
그렇지 않으면 이어지는 지점에서 DTS가 역행하고(non-monotonic DTS) 송출이 깨집니다.
업로드 시 remux가 하는 일은 재인코딩이 아니라 `-t <whole-frame duration>` 과
`-avoid_negative_ts make_zero` 로 **경계를 정리**하는 것입니다. 1080p 파일 기준
실측 약 119배속(I/O 한계)이라 90분 영상이 1분 이내입니다.

## 3. 업로드 시 변환 판단 (§7)

```
UPLOAD → ffprobe → plan_for()
                    ├─ 영상·음성 모두 프로필에 맞음 → remux (재인코딩 없음)
                    ├─ 음성만 다름               → 음성만 인코딩
                    ├─ 영상만 다름               → 영상만 인코딩
                    └─ 둘 다 다름               → 전체 인코딩
```

검사 항목: video codec(H.264), profile/level, pixel format(yuv420p), 해상도, fps,
keyframe 간격, audio codec(AAC), sample rate(48 kHz), 채널 수, 컨테이너.

- H.264 + AAC + 1080p30 파일: **재인코딩하지 않습니다.** 약 119배속 remux만.
- 그렇지 않은 파일: 필요한 스트림만 인코딩(약 3.45배속).

상태 표시: `업로드됨 → 분석 중 → 변환 중 → 사용 가능`(또는 `실패`).

## 4. 예약 (§8)

- 시간은 **UTC로 저장**하고, 화면에서만 사용자 시간대로 표시합니다.
- 요일 반복은 저장된 UTC 순간 + 브라우저가 알려준 오프셋으로 "그 사람의 시계에서
  같은 시각"을 계산합니다. 타임존 데이터베이스를 서버에 넣지 않은 대가로, DST가
  바뀌면 다시 저장할 때까지 한 시간 밀립니다.
- 스케줄러는 **아무 상태도 메모리에 두지 않습니다.** 30초마다 DB를 다시 읽고
  순수 함수 `schedule::decide()`에 묻습니다. 그래서 서버를 재시작해도 예약이
  그대로 복구됩니다 — 복구 코드가 따로 없습니다.
- 놓친 시작은 6시간까지만 따라잡습니다. 한 주 꺼져 있던 서버가 지난 화요일
  방송을 갑자기 시작하지 않게 하기 위해서입니다.
- 같은 occurrence는 두 번 시작하지 않습니다(`sched_last_run_at`).

## 5. 장애 복구 (§9)

| 상황 | 동작 |
| --- | --- |
| FFmpeg 비정상 종료 | 해당 방송만 supervisor가 backoff(2s→…→60s)로 재시작. 다른 방송은 무관 |
| 10회 연속 실패 | `FAILED`로 포기하고 이유를 남김 |
| 서버 재시작 | `desired_state=running` 인 방송만 복구 |
| 사용자 STOP | **절대 자동 재시작하지 않음**(`desired_state=stopped`) |
| 하드 kill 후 재시작 | 이전 실행이 남긴 FFmpeg를 pid로 확인·정리한 뒤 새로 시작 |

**재개 지점**: 워커가 매 tick(1초) 현재 항목·위치를 DB에 기록합니다. 복구 시
플레이리스트를 **재생 중이던 항목이 첫 번째가 되도록 회전**시켜 재생합니다.
엔진을 고치지 않고 재개하는 방법이고, 회전량을 저장해 두므로 대시보드는 여전히
사용자가 배열한 순서로 위치를 표시합니다.

항목 **안에서의** 초 단위 재개는 하지 않습니다. concat 데모서의 `inpoint`를
쓰면 가능하지만 엔진의 매니페스트 생성을 바꿔야 하므로, 현재는 항목 단위 재개 +
위치 기록(§9가 허용하는 checkpoint 방식)입니다.

## 6. 송출 설정 (§6)

기본값은 전부 `auto`이고, auto에서 서버는 **업로드 때 준비된 1080p30 파일을 그대로
보냅니다**(스트림 복사). 이것이 가장 안정적이고 CPU를 거의 쓰지 않습니다.

`auto`가 아닌 값은 저장·검증되지만 아직 **적용되지 않습니다**. 스트림 복사에서는
출력 해상도가 곧 파일의 해상도이기 때문에, 720p로 보내려면 송출 시점 재인코딩이
필요합니다(방송 하나당 코어 하나). UI가 이 사실을 그 자리에서 말합니다.

검증은 합니다: 해상도·fps 조합별 최소 비트레이트(YouTube 권고), 범위
(영상 1000–51000 kbps, 음성 64–512 kbps).

## 7. 방송 정보와 YouTube (§4, §5)

`stream_destinations.kind` 가 이 구분을 담습니다.

| kind | 할 수 있는 것 |
| --- | --- |
| `manual_rtmps` (현재) | 영상 송출. YouTube의 제목·설명·공개범위는 **바꾸지 않음** |
| `youtube_account` (예정) | OAuth로 연결해 라이브 생성·제목·설명·공개범위·썸네일 설정 |

그래서 방송 정보는 "247streams 내부 정보"입니다. UI가 제목 입력 옆에서 이를
명시합니다. `DestinationKind::can_publish_metadata()` 가 코드에서 같은 구분을
강제하므로, 나중에 메타데이터를 플랫폼에 보내는 기능을 붙일 때 "이 대상은 할 수
있는가"를 반드시 묻게 됩니다.

---

## 8. 방송이 안 보일 때 (진단)

대시보드가 "송출 중"인데 YouTube 채널에 나타나지 않을 때, 서버에서:

```bash
docker compose exec -T louver louver-server --diagnose
```

이 출력에 방송별로 다음이 나옵니다.

- desired / runtime 상태, 재시작 횟수, uptime
- 플레이리스트 위치, 지금/다음 영상
- **대상**: `scheme=rtmps host=a.rtmps.youtube.com path=/live2`
- **실행 중인 FFmpeg의 실제 명령줄** (`/proc/<pid>/cmdline`, 스트림 키는
  `[REDACTED:19자]`로 치환)
- concat 매니페스트의 내용
- 최근 기록 15줄

`docker compose logs -f louver` 에도 이제 방송별 줄이 나옵니다.

```
[louver][79a9baa6][info] 송출 대상: scheme=rtmps host=a.rtmps.youtube.com path=/live2 key=[REDACTED len=24 sha256:029b9036] 영상=3개
[louver][79a9baa6][info] RTMPS 전송을 시작합니다. 스트림 키 방식이므로 …
[louver][79a9baa6][info] spawn: …ffmpeg … -i …/manifest.txt -c copy -f flv … rtmps://a.rtmps.youtube.com/live2/••••••••
[louver][79a9baa6][info] 방송이 시작되었습니다
[louver][79a9baa6][ffmpeg] <FFmpeg가 stderr에 쓴 내용, 있을 때>
[louver][79a9baa6][warn] 방송이 중단되었습니다. 2초 후 자동으로 다시 연결합니다
```

`key=[REDACTED … sha256:xxxxxxxx]` 의 지문은 **키를 노출하지 않고** "어제
작동했던 그 키인가"를 두 로그 줄을 비교해 확인하기 위한 것입니다.

### "송출 중"이 뜻하는 것과 뜻하지 않는 것

| | |
| --- | --- |
| 뜻함 | FFmpeg가 RTMP(S) 연결을 열고 바이트를 쓰고 있다 (ingest가 publish를 받았다) |
| **뜻하지 않음** | **YouTube가 그 방송을 공개했다** |

247streams는 스트림 키 방식에서 **YouTube API를 호출하지 않습니다**. 라이브를
만들거나 "실시간 시작"을 누르지 않습니다. 그래서 대시보드가 송출 중이어도
Studio에서 공개 전환을 하지 않으면 채널에는 나타나지 않습니다. 카드와 로그가
이 사실을 그 자리에서 말합니다.

### 알려진 결함: 루프 이음새의 오디오 타임스탬프

`-stream_loop -1` 이 마지막 영상에서 처음으로 돌아갈 때, 받은 스트림의 오디오
DTS가 약 0.17 ms 뒤로 물러납니다(48 kHz 기준 8 샘플). AAC 프레임(1024 샘플)이
영상 길이와 정확히 나누어떨어지지 않기 때문입니다.

- **플레이리스트 기능 때문에 생긴 것이 아닙니다.** 영상 1개를 루프해도 같은
  지점에서 같은 결함이 나타납니다 — 즉 YouTube 송출이 정상 작동하던 버전에도
  있었습니다.
- 영상 *사이*의 이음새(같은 한 바퀴 안)는 깨끗합니다. 준비 단계의 whole-frame
  컷이 그 부분을 처리합니다.
- 한 바퀴에 한 번뿐입니다(1시간 29분 영상이면 89분마다 한 번).
- 고치려면 준비 단계에서 오디오를 영상 길이에 맞춰 자르거나 채워야 하고
  (`apad` + 정확한 `-t`), **이미 준비된 파일을 다시 준비해야** 합니다. 실송출
  경로를 건드리는 변경이므로 이번 긴급 조사에서는 하지 않았습니다.
