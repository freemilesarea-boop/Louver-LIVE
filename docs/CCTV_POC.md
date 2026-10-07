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

**stream copy는 제공하지 않습니다.** 서로 무관한 두 입력은 패킷 복사로 합칠 수
없으므로, CCTV URL이 설정된 방송은 계정 설정과 무관하게 encode 모드로 강제됩니다
(`runtime.rs`). CCTV 원본 오디오는 `-map` 에 아예 등장하지 않으므로 믹스되지
않고, 두 번째 트랙으로도 나가지 않습니다.

## 4. 지원하는 URL

- `http://`, `https://` **만**
- HLS(`.m3u8`)와 FFmpeg가 직접 읽을 수 있는 http/https 라이브 스트림
- 인증(아이디·비밀번호)이 필요한 CCTV는 **이번 PoC에서 지원하지 않습니다**
- RTSP·RTMP·file·data·concat 등은 거부

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
| 잘못된 주소 저장 시도 | 저장 자체가 400으로 거부 |
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
