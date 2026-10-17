# Traffic CCTV Live — PoC

실시간 교통 CCTV 영상 위에 247streams의 기존 플레이리스트 음악을 얹어 YouTube
Live까지 보낼 수 있는지 **검증하기 위한** 기능입니다. 정식 기능이 아니고, 그
전제로 작게 만들었습니다.

```
CCTV (HLS/HTTP)  ──► VIDEO ─┐
                            ├─ FFmpeg ─► RTMP ─► YouTube Live
플레이리스트 (기존)  ──► AUDIO ─┘
```

## 1. 먼저 — 기존 구조에 대한 정정

요청서에는 "247streams는 비주얼 소스와 음악을 결합하여 송출한다"고 적혀
있었지만, 실제 구조는 그렇지 않습니다. 확인한 사실:

- 방송은 `broadcast_items` 의 **영상 파일 목록**을 concat demuxer로 이어서
  송출합니다 (`FfmpegCommandBuilder::build_stream_args`).
- 소리는 그 영상 파일들이 가진 **자기 오디오 트랙**입니다.
- 즉 "비주얼 소스"와 "음악 플레이리스트"가 분리된 파이프라인은 **없습니다.**

그래서 이 PoC는 "음악 플레이리스트에 CCTV 화면을 붙인다"가 아니라, **기존
플레이리스트의 오디오만 쓰고 비디오를 CCTV로 교체**하는 형태로 구현했습니다.
결과물은 요청한 것과 같습니다: 화면은 CCTV, 소리는 내가 고른 기존 음악.

## 2. 무엇을 바꿨나

| 파일 | 변경 |
| --- | --- |
| `crates/louver-cloud/src/cctv.rs` | **신규.** URL 검증(SSRF 방어)과 `Test Connection` |
| `crates/louver-core/src/streaming/ffmpeg.rs` | `build_live_video_stream_args` **추가**. 기존 `build_stream_args` 는 손대지 않음 |
| `crates/louver-core/src/runtime.rs` | `set_live_video_source()` 추가. 설정되면 위 builder를 쓰고, `None` 이면 기존 경로 그대로 |
| `crates/louver-cloud/src/db.rs` | `broadcasts.cctv_url` 컬럼 1개 추가(nullable), 저장·검증 |
| `crates/louver-cloud/src/models.rs` | `Broadcast.cctv_url`, `BroadcastPatch.cctv_url` |
| `crates/louver-cloud/src/manager.rs` | worker 시작 시 URL 재검증 후 runtime에 전달 |
| `apps/server/src/api.rs`, `lib.rs` | `POST /api/cctv/test`, 생성 시 `cctv_url` 수용 |
| `apps/web/src/pages/BroadcastForm.tsx` | "2-1. 영상 소스 (테스트)" 섹션 |
| `apps/web/src/{cloud.ts,transport.ts,desktopTransport.ts}` | 타입과 `testCctv()` |

바꾸지 **않은** 것: 기존 FFmpeg 옵션, concat 파이프라인, 음악 재생·전환 로직,
RTMP 송출, YouTube OAuth/API, PayApp, 인증, 요금제, worker·process 관리,
스케줄러, 기존 UI 섹션 번호.

### DB 변경을 한 이유

하지 않을 수 없었습니다. 이 저장소의 `settings` 는 JSON 컬럼이 아니라
`resolution`·`fps`·`video_bitrate_kbps`·`audio_bitrate_kbps` 처럼 **개별 컬럼으로
평탄화**되어 있어서, 끼워 넣을 JSON 자리가 없습니다. 그래서 기존에 30번쯤 쓰인
것과 **같은 패턴**으로 nullable 컬럼 하나만 추가했습니다:

```rust
("cctv_url", "TEXT"),   // ensure_column, idempotent
```

`NULL` = "기존처럼 플레이리스트 영상을 쓴다" 이므로 이전 릴리스의 DB가 그대로
열리고 그대로 동작합니다. 테이블 추가도, 기존 컬럼 변경도, 데이터 이동도 없습니다.

## 3. FFmpeg 구조

`build_live_video_stream_args` 가 만드는 argv의 요지입니다.

```
-re -stream_loop -1 -f concat -safe 0 -i <manifest>        # 입력 0: 음악
-protocol_whitelist http,https,tcp,tls,crypto
-reconnect 1 -reconnect_streamed 1 -reconnect_delay_max 5
-rw_timeout 15000000 -i <CCTV URL>                          # 입력 1: 화면
-map 1:v:0 -map 0:a:0
<기존 video_encode_args> -r 30 -fps_mode cfr
-af aresample=async=1:first_pts=0
-c:a aac -b:a .. -ar .. -ac ..
-f flv -flvflags no_duration_filesize <RTMP URL>
```

틀리기 쉬운 지점 세 개를 의도적으로 그렇게 두었습니다.

1. **`-re` 와 `-stream_loop` 는 입력 0에만.** 둘 다 입력 옵션입니다. 실시간
   스트림을 `-re` 로 페이싱하거나 루프하는 것은 틀린 동작이고, 음악 쪽을
   페이싱하지 않으면 디스크가 읽는 속도로 플레이리스트가 흘러가 버립니다.
2. **reconnect·timeout 도 CCTV `-i` 앞에.** 프로토콜 옵션이기 때문입니다.
3. **`-protocol_whitelist` 에 `file` 이 없습니다.** HLS 플레이리스트는 자기
   세그먼트의 URL을 스스로 지정합니다. 이 목록이 없으면 외부에서 받은
   플레이리스트가 FFmpeg를 `file:` 로 돌려 서버 디스크를 읽게 할 수 있습니다.

### 서버 자원: 이 경로만 실시간 인코딩합니다

일반 방송은 `-c copy` 입니다 — 업로드 시 normalize 가 규격을 맞춰 두므로 송출은
파일 읽기와 소켓 쓰기뿐이고, 인코딩이 없습니다. CCTV 방송은 그렇게 할 수 없어
방송이 도는 동안 계속 H.264 를 인코딩합니다.

측정값 (2코어 고정, 제품의 실제 argv, 1080p30 소스):

| | speed |
| --- | --- |
| CCTV 1개 | 1.01x |
| CCTV 2개 동시 | **0.957x / 0.938x** |

1.0x 아래면 인코더가 영원히 따라잡지 못하고 muxer 큐가 자라 YouTube 가 불안정
스트림으로 봅니다. CPU 는 계정 사이에 나뉘지 않으므로 **두 고객 모두** 그렇게
됩니다.

### 화면은 프로필에서 상한이 걸립니다

카메라는 자기 크기를 스스로 고르고 이 경로는 실시간 인코딩이므로, 상한이 없으면
4K 카메라가 4K 로 인코딩됩니다. 2코어 실측:

| 3840×2160 카메라 | speed |
| --- | --- |
| 상한 없음 (이전) | **0.481x** — 실시간의 절반도 안 됨 |
| 프로필(1080p)로 상한 | **1.02x** |

`build_live_video_stream_args` 가 출력 앞에 이 필터를 넣습니다.

```
-vf scale=w='min(iw,1920)':h='min(ih,1080)'
    :force_original_aspect_ratio=decrease:force_divisible_by=2:flags=bicubic,
    setsar=1,format=yuv420p
```

- **`min(iw,W)`, 상수 `W` 가 아닙니다.** 프로필보다 작은 카메라는 손대지 않습니다.
  640×360 을 1080p 로 늘리면 인코더 비용은 세 배가 되고 화면 정보는 늘지 않으며,
  낮출 계정 설정도 없습니다 (§10 참조).
- **`pad` 없음** — normalize 와 다른 유일한 지점입니다. 프레임을 검은색으로
  채우면 상한이 아끼려던 비용이 그대로 돌아옵니다. 그래서 출력은 카메라의 화면
  비율을 따릅니다 (2560×1080 → 1920×810, 2048×1536 → 1440×1080).
- `min()` 안의 쉼표는 역슬래시가 아니라 `'...'` 로 보호합니다. 쉼표는 필터를
  구분하는 문자이고, 이 형태가 실제로 측정된 형태입니다.
- 송출 중 카메라 해상도가 바뀌는 경우는 **따로 처리할 것이 없습니다.** FFmpeg 가
  인코더를 연 크기로 자동 스케일합니다 — 이 필터가 있을 때와 없을 때 양쪽 모두
  측정했습니다. (`-reinit_filter 0` 을 넣을까 검토했지만, 측정해 보니 결과는
  같고 경고만 119줄 쌓여서 넣지 않았습니다.)

덕분에 아래 동시 인코딩 한도가 **의미 있는 수**가 됩니다. 한 자리가 "카메라가
보내는 만큼의 픽셀" 이 아니라 "프로필 크기의 인코딩 하나" 로 고정되기 때문입니다.

### 동시 인코딩 한도

요금제 한도와 **별개로** 기계 전체 한도가 있습니다
(`LOUVER_MAX_ENCODING_STREAMS`, 기본값 = 사용 가능 코어 ÷ 2, 최소 1).

- 인코딩 방송만 세고, 인코딩 방송만 거절합니다. 스트림 복사는 남의 카메라
  때문에 막히지 않습니다.
- 계정별이 아니라 **기계 전체**로 셉니다.
- 세는 것과 쓰는 것이 `claim_stream_slot` 의 같은 `BEGIN IMMEDIATE` 트랜잭션
  안에 있으므로, 동시에 도착한 두 시작이 둘 다 마지막 자리를 받을 수 없습니다.
- `recover_all` 도 같은 한도를 지킵니다. 재시작이 그 검사를 우회하면 부팅이
  모든 CCTV 방송을 한꺼번에 되살려 예산을 넘기는데, 아무도 보고 있지 않은
  시점이라 클릭보다 더 나쁩니다. 한도를 넘는 방송은 `desired_state` 를 그대로
  둔 채 보류하고 그 이유를 방송 로그에 남깁니다 — 자리가 생기면 시작됩니다.
- 거절은 HTTP **503** 입니다. 402(요금제)가 아닙니다 — 돈을 더 내도 이 서버에
  코어가 생기지 않습니다. 507(저장공간)도 아닙니다 — 방송이 끝나면 저절로
  풀리므로 "잠시 후 다시" 가 사실입니다.

**stream copy는 제공하지 않습니다.** 서로 무관한 두 입력은 패킷 복사로 합칠 수
없으므로, CCTV URL이 설정된 방송은 계정 설정과 무관하게 encode 모드로 강제됩니다
(`runtime.rs`). CCTV 원본 오디오는 `-map` 에 아예 등장하지 않으므로 믹스되지
않고, 두 번째 트랙으로도 나가지 않습니다.

## 4. 지원하는 URL

- `http://`, `https://` **만**
- HLS(`.m3u8`)와 FFmpeg가 직접 읽을 수 있는 http/https 라이브 스트림
- 인증(아이디·비밀번호)이 필요한 CCTV는 **이번 PoC에서 지원하지 않습니다**
- RTSP·RTMP·file·data·concat 등은 거부

### 시청 페이지 링크는 거부합니다

YouTube·YouTu.be·Twitch·Vimeo·TikTok·Instagram·Facebook·치지직·네이버 TV·
카카오 TV·아프리카TV·SOOP 의 주소는 `cctv::validate` 가 **저장 시점에** 거부하고,
어느 사이트인지와 왜 안 되는지를 함께 알려줍니다.

이유는 간단합니다. FFmpeg 는 URL 을 열어 컨테이너를 기대하는데
`youtube.com/watch?v=…` 는 HTML 입니다. 그래서 FFmpeg 는
`Invalid data found when processing input` 을 돌려주고, 그 문장은 사실이지만
고객에게는 아무것도 알려주지 않습니다. 더 나쁜 것은 그 일이 **시작 시점에**
일어난다는 점입니다 — 주소는 멀쩡히 저장되고, 방송은 실패하고, supervisor 가
60초마다 영구히 재시도합니다.

이 페이지들에서 재생 가능한 스트림을 뽑으려면 별도 도구(`yt-dlp` 류)가 필요한데
이 서비스는 그것을 포함하지 않고, 해당 사이트 약관도 일반적으로 허용하지
않습니다. 그래서 답은 "안 됩니다" 이고, 그 말을 하는 자리는 고객이 아직 입력
칸을 보고 있는 저장 시점입니다.

`googlevideo.com` 같은 CDN 호스트는 **거부하지 않습니다**. 서명된 미디어 URL 은
FFmpeg 가 실제로 읽을 수 있는 스트림이므로 거부하면 틀린 동작입니다. 또
`notyoutube.com` 처럼 이름만 겹치는 도메인도 거부하지 않습니다 — 도메인 자신과
그 서브도메인만 매칭합니다.

## 5. 보안

사용자가 입력한 주소를 **서버가** 여는 구조이므로, 그대로 두면 설계상 SSRF입니다.
`cctv::validate` 가 저장 시점과 방송 시작 시점 양쪽에서 다음을 거부합니다.

| 거부 대상 | 예 |
| --- | --- |
| http/https 외 프로토콜 | `file:///etc/passwd`, `rtsp://…` |
| 루프백 | `127.0.0.1`, `127.1`, `::1`, `::ffff:127.0.0.1`, `localhost` |
| 사설망 | `10/8`, `172.16/12`, `192.168/16`, `fc00::/7` |
| 링크 로컬·메타데이터 | `169.254.169.254`, `fe80::/10`, `metadata.google.internal`, `*.internal` |
| CGNAT·예약·문서용·멀티캐스트 | `100.64/10`, `0/8`, `240/4`, `203.0.113/24` … |
| URL 내 자격증명 | `http://trusted.example@127.0.0.1/` |
| 공백·제어문자, 2048자 초과, 잘못된 포트 | |

호스트는 **resolve 후 모든 주소**를 검사합니다. 하나라도 비공개면 거부합니다.

**command injection**: URL은 셸에 들어가지 않습니다. 이 저장소의 모든 FFmpeg
실행은 argv 벡터(`Command::args`)이고(§60), CCTV 경로도 동일합니다. 공백·개행이
들어간 주소는 그 전에 거부되므로 로그 한 줄도 쪼갤 수 없습니다.

**남아 있는 한계(정직하게)**: 검사 시점에는 공인 IP로, FFmpeg가 접속할 때는
사설 IP로 응답하는 DNS rebinding은 막지 못합니다. 막으려면 검사한 주소로 연결을
고정해야 하는데 FFmpeg가 그 방법을 제공하지 않습니다. PoC 범위에서 제외하고
기록해 둡니다.

## 6. 장애 처리

| 상황 | 동작 |
| --- | --- |
| 최초 연결 실패 | FFmpeg가 즉시 종료 → 기존 supervisor가 backoff 후 재시작, 사유를 방송 이벤트에 기록 |
| 스트림 중단·정지 | `-rw_timeout` 15초 → 종료 → supervisor 재시작 |
| 짧은 끊김 | FFmpeg `-reconnect` 가 프로세스 안에서 복구(최대 5초 지연) |
| 형식·주소가 잘못된 경우 | 저장 자체가 400으로 거부 (scheme, 자격증명, 내부망·메타데이터 주소, 시청 페이지 링크) |
| 형식은 맞지만 읽을 수 없는 주소 | `validate` 는 통과합니다 — "읽을 수 있는 스트림인가" 는 주소 모양으로 알 수 없습니다. 그래서 **UI 가 `연결 테스트` 성공 없이는 저장을 막습니다**(`BroadcastForm.tsx`). 테스트 후 주소를 고치면 그 결과는 무효가 되고 다시 테스트해야 합니다 |
| 시작 시 주소가 부적합 | 그 방송만 시작 실패 + 이벤트 로그, 다른 방송 무영향 |

방송 하나가 자기 worker 스레드와 자기 FFmpeg 프로세스를 갖는 기존 구조를 그대로
쓰기 때문에, CCTV가 끊겨도 **그 방송만** 실패·재연결합니다.

## 7. 검증 결과

| 테스트 | 내용 | 결과 |
| --- | --- | --- |
| `louver-cloud` `cctv::tests` (14) | SSRF 거부 목록, 프로토콜, 자격증명, 포트, ffprobe JSON 해석, 오류 분류 | PASS |
| `louver-core` `streaming::ffmpeg` (신규 7) | map 순서, `-re`/`-stream_loop` 위치, whitelist에 `file` 없음, timeout, 비-copy, 키 마스킹 | PASS |
| `louver-core` `tests/live_video_source.rs` (2) | **실제 FFmpeg E2E**: 로컬 HTTP로 서빙한 HLS + 음악 → FLV | PASS |
| `louver-server` `http_api` (신규 4) | `/api/cctv/test` 거부·인증, `cctv_url` 저장·해제, 내부 주소 저장 거부 | PASS |

E2E 테스트가 단정하는 두 가지:

- 출력 화면 크기가 **카메라의 640×360** 이고 플레이리스트의 1920×1080이 아니다 →
  비디오는 CCTV에서 왔다.
- 출력 오디오의 **440 Hz(음악) 밴드가 1 kHz(카메라) 밴드보다 약 27 dB 높다** →
  소리는 플레이리스트에서 왔고 카메라 오디오는 쓰이지 않았다. (같은 파이프라인을
  `-map 1:a` 로 돌린 비교군은 −20 dB. 두 경우가 47 dB 떨어져 있어 임계값 15 dB는
  어느 쪽에도 가깝지 않습니다.)

## 8. 사용 방법

1. 웹에서 로그인 → **영상** 탭에서 음악으로 쓸 영상을 업로드 (기존과 동일)
2. **방송** 탭 → `방송 만들기`
3. `2. 플레이리스트` 에서 음악으로 쓸 영상을 고름 — **소리는 여기서 나갑니다**
4. `2-1. 영상 소스 (테스트)` → `Traffic CCTV (Test)` 선택
5. `CCTV 주소` 에 HLS URL 입력 (예: `https://…/live/stream.m3u8`)
6. `연결 테스트` → `SUCCESS · 연결됨 · h264 · 1280×720 · 30fps` 확인
7. `3. 송출 대상` 에서 YouTube 계정 또는 스트림 키 선택 (기존과 동일)
8. `방송 만들기` → 목록에서 `시작`
9. YouTube Live에서 CCTV 화면 + 내 음악 확인

되돌리려면 같은 화면에서 `플레이리스트 영상 (기본)` 으로 바꾸고 저장하면 됩니다.

## 9. PoC에 넣지 않은 것

| 항목 | 이유 |
| --- | --- |
| Preview | 범위가 커짐. 요청서도 "쉽게 가능하면"이었고, 최우선 목표는 실제 Live 확인 |
| CCTV 검색 API·지도·자동 검색 | 요청서에서 제외 |
| 인증 필요한 CCTV | URL 자격증명은 SSRF 우회 경로라 거부 |
| RTSP | FFmpeg는 읽지만 이번 범위는 http/https |
| 복잡한 failover | 요청서에서 제외. 기존 supervisor 재시작만 사용 |
| `check_playlist_joinable` 완화 | CCTV 모드에서는 오디오만 쓰므로 비디오 규격 일치 검사가 과하지만, 완화는 기존 로직 변경이라 하지 않음 |
| 읽을 수 없는 주소에 대한 재시도 포기(give-up) | supervisor 는 최대 60초 backoff 로 영구 재시도합니다. 카메라가 돌아올 수 있으므로 24/7 서비스에서는 그 편이 맞지만, 영구히 읽을 수 없는 주소에는 과합니다. 저장 시점 차단으로 실제 경로는 막았고, supervisor 변경은 모든 방송에 영향을 주므로 범위 밖으로 두었습니다 |
| 업스케일 | 프로필보다 작은 카메라는 그대로 보냅니다(`min(iw,W)`). 640×360 교통 카메라를 1080p 로 늘리면 인코더 비용이 세 배가 되는데 화면 정보는 늘지 않고, 되돌릴 계정 설정도 없습니다 — 아래 참조 |
| `StreamSettings` 를 **작동하게** 만드는 것 | 대신 **없앴습니다** — §10 |

## 10. 읽히지 않던 송출 설정을 없앴습니다

`StreamSettings` (`resolution`·`fps`·`video_bitrate_kbps`·`audio_bitrate_kbps`)
는 검증되고 저장되었지만 **어디서도 읽히지 않았습니다.** `is_auto()` 는 호출자가
없었고, 클라우드는 모든 방송을 `P1080p30` 으로 만듭니다 (`manager.rs`). 폼의
"고급 설정" 은 인코더가 지키지 않는 약속이었고, 거기엔 "재인코딩 송출이 추가되면
적용됩니다" 라는 문구까지 붙어 있었습니다.

### 작동하게 만들지 않고 없앤 이유

작동하게 만드는 것은 정리가 아니라 새 기능입니다. 세 가지가 막습니다.

1. **60fps 프로필이 없습니다.** `OutputProfile` 에는 `P1080p30` 과 `P720p30`
   둘뿐입니다. 폼의 `60` 은 어떤 코드로도 지킬 수 없는 값이었습니다.
2. **비트레이트 인자가 없습니다.** 비트레이트는 프로필이 고정으로 들고 있고
   (1080p30 = 10 Mbps, 720p30 = 4 Mbps), core 빌더에 재정의 경로가 없습니다.
3. **복사 경로에서는 해상도를 지킬 수 없습니다.** 일반 방송은 준비된 파일을
   `-c copy` 로 보냅니다. 720p 를 지키려면 실시간 재인코딩을 해야 하는데, 그것이
   바로 §3 의 동시 인코딩 한도가 막으려는 비용입니다. 즉 설정을 살리면 CCTV 방송에만
   적용되고 대다수 방송에서는 조용히 무시되는, 더 헷갈리는 제품이 됩니다.

덧붙여 `liveStreams.insert` 에 우리는 이미 `cdn.resolution: "variable"`,
`frameRate: "variable"` 을 보냅니다 (`youtube/api.rs`). 방송별 해상도 약속은
YouTube 쪽에도 애초에 없었습니다.

### 바꾼 것

| 파일 | 변경 |
| --- | --- |
| `crates/louver-cloud/src/models.rs` | `StreamSettings` 와 `Broadcast.settings`·`BroadcastPatch.settings` 삭제 |
| `crates/louver-cloud/src/db.rs` | 네 컬럼의 읽기·쓰기·검증 삭제. **컬럼 자체는 유지** |
| `apps/server/src/api.rs` | `NewBroadcast.settings` 삭제 |
| `apps/web/src/cloud.ts` | `StreamSettings`·`AUTO_SETTINGS` 와 세 군데의 필드 삭제 |
| `apps/web/src/pages/BroadcastForm.tsx` | "고급 설정" 토글과 네 입력칸 삭제. 카드는 실제 동작을 설명 |
| `apps/web/src/desktopTransport.ts` | 데스크톱이 채워 넣던 `AUTO_SETTINGS` 삭제 |

### 컬럼을 남긴 이유

`ensure_column` 이 이전 릴리스의 DB 를 여는 수단이고, 운영 중인 서비스에서
컬럼을 DROP 하면 테이블을 다시 씁니다. 아무도 읽지 않는 네 값 때문에 할 일이
아닙니다. 그리고 "읽지 않는다" 는 말이 정확히 그 뜻이어야 하므로, **편집이
그 값을 덮어쓰지 않는지**를 테스트로 고정했습니다
(`migration.rs::an_edit_does_not_overwrite_the_sending_settings_nobody_reads_any_more`).
720p 를 골라 두었던 고객의 행은 그대로 남습니다.

### 배포 순서 주의

`NewBroadcast` 에는 `deny_unknown_fields` 가 없으므로, 구버전 웹 번들이
`settings` 를 계속 보내도 400 이 아니라 무시됩니다. 반대로 API 응답에서
`settings` 가 사라지므로, 프런트를 먼저 올리고 API 를 나중에 올리는 순서가
안전합니다 (PR #11 의 독립 배포 구조를 쓸 때).
