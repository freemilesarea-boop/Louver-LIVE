# Louver Live

여러 개의 완성된 음악 영상을 넣어두면, 지정한 시간 동안 자동으로 순차 반복하며
YouTube Live로 송출하는 데스크톱 프로그램입니다.

OBS 같은 범용 방송 프로그램이 아닙니다. 기본 사용법은 세 단계뿐입니다.

1. 영상 넣기
2. YouTube 스트림 키 입력
3. 방송 시작

이게 전부입니다. **Google 로그인, OAuth, YouTube Data API, Google Cloud 설정은
하나도 필요하지 않습니다.** 방송은 스트림 키 하나로 RTMPS 송출합니다.

### 고급 기능 (선택)

방송 제목·설명·태그를 앱에서 바꾸거나 자동 라이브 채팅을 쓰고 싶을 때만,
`설정 → YouTube 고급 기능`에서 `[YouTube 계정 연결]`을 누릅니다. 버튼을 누르면
브라우저에서 Google 계정을 고르고 허용하면 끝입니다 — Google Cloud도, Client ID도,
카드 등록도 없습니다. 무료 한도 안에서만 동작하므로 요금도 붙지 않습니다.
선택 기능이며 연결하지 않아도 방송은 100% 정상 동작합니다. 연결 후 OAuth나 API에서 오류가
나더라도 송출은 멈추지 않고, 제목·채팅 기능만 비활성화됩니다.
준비 방법은 [YOUTUBE_SETUP.md](YOUTUBE_SETUP.md).

## 직접 써보기

```bash
npm install
npm run app
```

실제 앱 창이 열립니다. 브라우저 미리보기가 아니라 Rust 백엔드, SQLite, FFmpeg,
스케줄러, 스트림 감독까지 모두 도는 진짜 앱입니다. 순서대로 무엇을 눌러보면
되는지는 **[LOCAL_TEST.md](LOCAL_TEST.md)** 에 있습니다.

컴퓨터만 켜져 있으면 나머지는 프로그램이 알아서 합니다. 인터넷이 잠깐 끊겨도,
FFmpeg가 비정상 종료되어도, PC가 재부팅되어도 방송은 자동으로 복구됩니다.

---

## 왜 가벼운가

방송 중에는 영상을 다시 인코딩하지 않습니다. 무거운 작업은 영상을 처음 등록할
때 한 번만 하고, 실제 방송은 이미 표준화된 파일을 그대로 흘려보냅니다
(stream copy).

실측값 (1080p30 프로필, 이 저장소의 `npm run soak`):

| 항목 | 측정값 |
| --- | --- |
| FFmpeg CPU | **1.5 – 1.8 %** |
| FFmpeg 메모리 | 61.7 MB (안정 후 고정) |
| 메모리 증가 (안정 구간) | **0.00 %** → 24시간 환산 0 % |
| 타임스탬프 오류 | 0 |

자세한 측정 방법과 환경은 [BENCHMARK.md](BENCHMARK.md)를 보세요.

---

## 설치

### 사용자

[릴리스](https://github.com/freemilesarea-boop/Louver-LIVE/releases)에서
운영체제에 맞는 파일을 내려받아 실행합니다. FFmpeg는 프로그램에 들어 있으므로
따로 설치하지 않으셔도 됩니다.

| 쓰시는 컴퓨터 | 받으실 파일 |
| --- | --- |
| Windows 10 또는 11 (64비트) | `.exe` — 쉬운 쪽입니다. `.msi`는 회사에서 일괄 배포할 때 씁니다 |
| Mac — M1·M2·M3 이후 | 파일 이름에 `aarch64`가 있는 `.dmg` |
| Mac — 2020년 이전 (Intel) | 파일 이름에 `x64`가 있는 `.dmg` |
| Linux — 우분투·민트·데비안 | `.deb` |
| Linux — 그 밖의 배포판 | `.AppImage` (내려받아 실행 권한만 주면 됩니다) |

**Mac이 어느 쪽인지 모르시겠다면**: 화면 왼쪽 위 사과 모양 → `이 Mac에 관하여`.
"칩"이라고 적혀 있으면 Apple Silicon, "프로세서"라고 적혀 있으면 Intel입니다.

서명 인증서가 아직 없는 빌드를 받으셨다면 처음 한 번 경고가 뜹니다.
Mac은 응용 프로그램 폴더에서 **제어(Control) 키를 누른 채 클릭 → 열기 → 열기**,
Windows는 **추가 정보 → 실행**을 누르시면 됩니다. 다음부터는 뜨지 않습니다.

### 개발자

```bash
git clone https://github.com/freemilesarea-boop/Louver-LIVE.git
cd Louver-LIVE
npm install
npm run sidecar      # FFmpeg/ffprobe 사이드카 준비
```

필요한 것: Node.js 18+, Rust 1.77+, 그리고 Tauri의
[플랫폼별 사전 요구사항](https://tauri.app/start/prerequisites/).

---

## 개발 실행

```bash
npm run tauri:dev    # 데스크톱 앱 (Tauri + Rust + React)
npm run dev          # UI만 브라우저에서 (인메모리 목 백엔드 사용)
```

`npm run dev`는 Rust 없이 UI만 띄웁니다. 화면 작업에는 편하지만 실제 송출은
되지 않습니다.

## 테스트

```bash
npm run verify       # 아래 전부를 순서대로 실행
```

개별 실행:

```bash
npm run typecheck    # TypeScript strict
npm run lint         # ESLint
npm run test         # 프론트엔드 단위 테스트 (Vitest)
npm run test:e2e     # UI 전체 시나리오 (§58)
npm run test:rust    # Rust 전체 (단위 + 통합)
npm run test:media   # FFmpeg 실물 미디어 파이프라인 테스트
npm run soak -- --duration 1h    # 장시간 안정성 (§55)
```

테스트용 영상은 실행할 때 FFmpeg로 생성합니다. 저작권 있는 영상은 저장소에
포함하지 않습니다.

## 빌드

```bash
npm run build        # 사이드카 준비 → 프론트엔드 빌드 → Tauri 번들
```

배포용 빌드는 반드시 정식 정적 빌드를 사용해야 합니다:

```bash
node scripts/fetch-ffmpeg.mjs --require-download --force
```

이 옵션 없이 빌드하면 개발 편의를 위해 시스템에 설치된 FFmpeg를 복사합니다.
그 바이너리는 재배포 라이선스가 확인되지 않았고, 동적 링크라 다른 컴퓨터에서
실행되지 않습니다. ([LICENSES.md](LICENSES.md) 참고)

릴리스 빌드에는 두 겹의 잠금장치가 있습니다. `--require-download`는 내려받기에
실패하면 그 자리에서 멈추고, 이미 놓여 있던 개발용 사이드카도 받아들이지
않습니다. 그리고 `node scripts/ffmpeg-manifest.mjs --check`가 실제 바이너리를
읽어 라이선스·링크 방식·인코더·프로토콜을 확인하고, 동적 링크된 것이면 빌드를
중단시킵니다. 네 플랫폼 설치 파일을 만드는 전체 절차는
[RELEASING.md](RELEASING.md)에 있습니다.

---

## 스트림 키 입력

1. YouTube Studio → 만들기 → 실시간 스트리밍 시작
2. "스트림 키" 항목의 값을 복사
3. Louver Live → 설정 → 기본 송출 → YouTube 스트림 키에 붙여넣고 저장

스트림 키는 운영체제의 보안 저장소에 저장됩니다.

- macOS: 키체인
- Windows: 자격 증명 관리자

SQLite, JSON, 로그 파일 어디에도 평문으로 저장되지 않습니다. 화면에는 항상
`••••••••`로 표시되며, `[보기]`를 눌러 확인할 때도 한 번 더 확인을 거칩니다.

## 첫 방송

1. **플레이리스트** 탭에서 플레이리스트를 만들고 `+ 영상 추가`로 영상을 넣습니다.
2. 영상이 방송 규격과 다르면 `최적화 필요`로 표시됩니다.
   `방송용으로 최적화`를 누르면 필요한 저장 공간을 먼저 보여주고, 확인 후
   변환합니다. **원본 파일은 절대 수정되지 않습니다.**

   최적화는 **필요한 것만** 합니다. 이미 방송 규격(H.264 / 지정 해상도 ·
   프레임레이트 / yuv420p / 48kHz 스테레오 AAC)인 영상은 다시 인코딩하지 않고
   컨테이너만 다시 씁니다 — 60초 영상 기준 **0.2초, 실시간의 200배 이상**.
   화면만 또는 소리만 어긋난 영상은 어긋난 쪽만 인코딩합니다.

   | 영상 상태 | 하는 일 | 60초 기준 (CPU, libx264) |
   | --- | --- | --- |
   | 이미 방송 규격 | 컨테이너만 다시 씀 | 0.2초 (이전 15.6초) |
   | 소리만 다름 | 소리만 인코딩 | 1.7초 (이전 15.2초) |
   | 화면·소리 모두 다름 | 둘 다 인코딩 | 15.7초 (변화 없음) |

   진행 중에는 지금 무엇을 하고 있는지, 처리 속도와 남은 시간, 어떤 엔진
   (NVIDIA GPU · Intel Quick Sync · AMD GPU · CPU)을 쓰는지 함께 보여줍니다.
   변환이 필요 없는 영상을 먼저 처리하므로, 대부분 이미 규격에 맞는
   라이브러리는 거의 즉시 준비됩니다.
3. 드래그해서 순서를 바꾸고, Sequential 또는 Shuffle Once를 고릅니다.
4. **방송 예약** 탭에서 시작/종료 시간과 요일을 정합니다.
   `20:00 → 08:00`처럼 자정을 넘기는 예약도 됩니다.
5. **대시보드**에서 `방송 시작`을 누르거나, 예약 시간이 되기를 기다립니다.

스트림 키 없이 전체 파이프라인을 시험해보려면 대시보드의 `로컬 테스트`를
누르세요. 실제 송출 대신 로컬 파일로 내보내며, 화면에는 `LIVE`가 아니라
`TEST`로 표시됩니다.

---

## 문제 해결

### 방송이 시작되지 않습니다

`방송 시작`을 누르면 7가지를 먼저 점검하고, 실패한 항목을 오류 코드와 함께
보여줍니다. 대부분은 아래 중 하나입니다.

| 증상 | 원인 | 해결 |
| --- | --- | --- |
| `LL-STREAM-003` | 최적화되지 않은 영상이 있음 | 플레이리스트에서 `방송용으로 최적화` 실행 |
| `LL-STREAM-004` | 플레이리스트가 비어 있음 | 영상 추가 |
| `LL-STREAM-007` | 스트림 키 없음 |
| `LL-STREAM-008` | YouTube가 스트림 키를 거부함 | 설정에서 입력 |
| `LL-NETWORK-001` | 인터넷 연결 없음 | 네트워크 확인 |
| `LL-CONFIG-002` | FFmpeg 사이드카 없음 | 재설치, 또는 `npm run sidecar` |
| `LL-LICENSE-001` | 라이선스 없음 | 설정 → 라이선스에서 등록 |

### 방송 중 화면이 `재연결 중`으로 바뀝니다

인터넷이 끊겼거나 FFmpeg가 종료된 상태입니다. 프로그램이 2초 → 5초 → 10초 →
20초(최대 60초) 간격으로 자동 재시도합니다. 사용자가 직접 종료한 경우에는
재시도하지 않습니다.

인터넷이 끊기면 FFmpeg가 곧바로 종료되지 않고 멈춰 있는 경우가 많습니다.
이런 상황도 감지하기 위해 30초 동안 전송이 없으면 자동으로 재연결합니다.
실측: 60초 동안 네트워크를 차단했을 때 35초 만에 `재연결 중`으로 바뀌었고,
네트워크 복구 후 4초 만에 방송이 재개되었습니다.

### CPU 사용량이 높습니다

설정 → 고급 → 개발자 모드에서 `Streaming Mode`를 먼저 확인하세요.
`STREAM COPY`이면 영상을 재인코딩하지 않으므로 CPU 사용량이 낮아야 합니다
(실측 0.5~0.7%). `COMPATIBILITY MODE`로 표시된다면 실시간 재인코딩 중이므로
CPU 사용량이 높은 것이 정상입니다. 설정 → 송출 → 송출 모드에서 바꿀 수 있습니다.

개발자 모드에서는 실제로 실행 중인 FFmpeg 명령도 확인할 수 있습니다.
스트림 키는 `••••••••`로 가려져 표시됩니다.

### 컴퓨터가 절전 모드로 들어갑니다

방송 중에는 자동으로 절전을 차단합니다. 다만 **노트북 덮개를 닫으면**
운영체제가 강제로 절전에 들어가므로 방송이 끊깁니다. 이것은 우회하지 않습니다.

### 저장 공간이 부족합니다

최적화된 영상은 1080p30 기준 시간당 약 2.8 GB를 사용합니다 (영상 6 Mbps +
소리 192 kbps). 다시 인코딩하지 않고 컨테이너만 바꾼 영상은 원본과 비슷한
크기가 되므로, 필요 공간은 원본 크기도 함께 계산합니다. 설정 → 저장공간
에서 캐시 크기를 확인하고 `캐시 전체 삭제`로 정리할 수 있습니다. 현재
플레이리스트가 쓰고 있는 파일이 있으면 삭제 전에 경고합니다.

### 창을 닫아도 방송이 계속됩니다

의도된 동작입니다. 방송 중 창을 닫으면 트레이로 숨겨지고 방송은 유지됩니다.
완전히 끝내려면 트레이 아이콘 → `프로그램 종료`를 사용하세요.

---

## 로그 위치

| OS | 경로 |
| --- | --- |
| Windows | `%APPDATA%\LouverLive\logs\` |
| macOS | `~/Library/Application Support/LouverLive/logs/` |
| Linux | `~/.local/share/LouverLive/logs/` |

정확한 macOS 경로는 `~/Library/Application Support/LouverLive/logs/app.log`
입니다. `~/Library/Logs/` 아래에는 아무것도 기록하지 않습니다.

`app.log`(앱), `stream.log`(방송), `ffmpeg.log`(FFmpeg 출력) 세 가지가 있으며
각각 최대 10 MB × 5개까지 보관하고 오래된 것부터 삭제합니다.
설정 → 고급 → `로그 폴더 열기`로 바로 열 수 있습니다.

**스트림 키는 어떤 로그에도 기록되지 않습니다.** 로거에 들어가는 모든 문자열이
마스킹을 거치므로, 호출하는 쪽에서 실수할 여지가 없습니다.

같은 폴더에 데이터베이스(`louver.db`), 최적화 캐시(`cache/`), 세션 상태
(`session.json`)가 함께 있습니다.

### 방송 준비 과정 로그

YouTube 방송을 준비하는 동안 각 단계가 `app.log`에 `_START` / `_OK` / `_FAIL`
세 가지로 남습니다. 실패했을 때 **어느 요청이** 거부됐는지 바로 알 수 있습니다.

| 이벤트 | 하는 일 |
| --- | --- |
| `YOUTUBE_TOKEN_REFRESH_*` | 저장된 로그인으로 Google 인증 갱신 |
| `YOUTUBE_BROADCAST_LIST_*` | 이 예약에 쓸 방송이 이미 있는지 확인 (`mine=true` 하나만 보내고, 이 예약 시각 ±15분인 방송을 앱 안에서 고릅니다) |
| `YOUTUBE_BROADCAST_INSERT_*` | 없으면 방송을 새로 만듦 |
| `YOUTUBE_STREAM_LIST_*` | 저장된 스트림 키가 가리키는 수신 지점 찾기 |
| `YOUTUBE_BROADCAST_BIND_*` | 방송과 수신 지점 연결 |
| `YOUTUBE_METADATA_APPLY_*` | 제목·설명·태그·카테고리·공개범위 적용 |
| `YOUTUBE_STREAM_ACTIVE_WAIT` / `YOUTUBE_STREAM_ACTIVE` | YouTube가 영상을 받기 시작했는지 |
| `YOUTUBE_BROADCAST_TRANSITION_*` | 방송을 LIVE로 전환 |

각 줄에는 그 시도를 누가 시작했는지(`origin=manual` 또는 `origin=scheduled`)가
붙습니다. 같은 로그 안에서 수동 방송과 예약 방송의 순서를 나란히 비교할 수
있게 하기 위한 것입니다.

`_FAIL` 줄은 Google이 답한 그대로를 담습니다 — API 메서드, HTTP 상태, Google의
error reason, Google의 메시지. 예:

```
YOUTUBE_BROADCAST_INSERT_FAIL: origin=scheduled · LL-YOUTUBE-004 ·
liveBroadcasts.insert HTTP 403 reason=liveStreamingNotEnabled ·
The user is not enabled for live streaming.
```

**액세스 토큰, 리프레시 토큰, 클라이언트 시크릿, 스트림 키는 이 줄들 중 어디에도
기록되지 않습니다.** 방송 ID와 스트림 ID만 남으며, 둘 다 비밀이 아닙니다.

---

## 오류 코드

| 코드 | 의미 |
| --- | --- |
| `LL-MEDIA-001` | 영상 정보를 읽지 못함 (ffprobe 실패) |
| `LL-MEDIA-002` | 지원하지 않는 형식 |
| `LL-MEDIA-003` | 파일을 찾을 수 없음 |
| `LL-MEDIA-004` | 영상 트랙 없음 |
| `LL-MEDIA-005` | 최적화 실패 |
| `LL-MEDIA-006` | 최적화가 취소됨 |
| `LL-STREAM-001` | 방송 엔진을 시작하지 못함 |
| `LL-STREAM-002` | 방송이 예기치 않게 중단됨 |
| `LL-STREAM-003` | 최적화되지 않은 영상이 있음 |
| `LL-STREAM-004` | 플레이리스트가 비어 있음 |
| `LL-STREAM-005` | 이미 방송 중 |
| `LL-STREAM-006` | 허용되지 않는 상태 전환 |
| `LL-STREAM-007` | 스트림 키 없음 |
| `LL-STREAM-008` | YouTube가 스트림 키를 거부함 |
| `LL-STREAM-009` | 플레이리스트를 찾을 수 없음 (삭제됨) |
| `LL-NETWORK-001` | 인터넷 연결 없음 |
| `LL-NETWORK-002` | RTMPS 서버 연결 실패 |
| `LL-STORAGE-001` | 저장 공간 부족 |
| `LL-STORAGE-002` | 캐시 손상 |
| `LL-STORAGE-003` | 파일 입출력 오류 |
| `LL-DB-001` | 데이터베이스를 열지 못함 |
| `LL-DB-002` | 마이그레이션 실패 |
| `LL-DB-003` | 데이터 조회/저장 실패 |
| `LL-SEC-001` | 보안 저장소를 사용할 수 없음 |
| `LL-SEC-002` | 저장된 스트림 키 없음 |
| `LL-LICENSE-001` | 라이선스 없음 |
| `LL-LICENSE-002` | 라이선스 서명이 올바르지 않음 |
| `LL-LICENSE-003` | 라이선스 파일 형식 오류 |
| `LL-LICENSE-004` | 라이선스 만료 |
| `LL-LICENSE-005` | 다른 컴퓨터에 등록된 라이선스 |
| `LL-SCHED-001` | 예약 시간이 올바르지 않음 |
| `LL-SCHED-002` | 요일이 선택되지 않음 |
| `LL-SCHED-003` | 예약에 연결된 플레이리스트를 찾을 수 없음 |
| `LL-SCHED-004` | 예약된 플레이리스트에 방송 가능한 영상이 없음 |
| `LL-CONFIG-001` | 설정 값이 올바르지 않음 |
| `LL-YOUTUBE-001` | YouTube 계정이 연결되지 않음 |
| `LL-YOUTUBE-002` | YouTube 로그인이 만료됨 (동의 화면에서 받은 코드 교환 실패) |
| `LL-YOUTUBE-AUTH-REFRESH` | 저장된 로그인으로 Google 인증을 갱신하지 못함 |
| `LL-YOUTUBE-003` | 진행 중인 라이브를 찾지 못함 |
| `LL-YOUTUBE-004` | YouTube API 호출 실패 |
| `LL-YOUTUBE-005` | YouTube API 일일 사용량 초과 |
| `LL-YOUTUBE-006` | 방송 정보가 YouTube 제한을 넘음 |
| `LL-CHAT-001` | 채팅 전송이 너무 잦음 |
| `LL-CHAT-002` | 방송의 실시간 채팅이 꺼져 있음 |
| `LL-CHAT-003` | 방송의 실시간 채팅이 종료됨 |
| `LL-CHAT-004` | 채팅 메시지가 200자를 넘음 |
| `LL-CONFIG-002` | FFmpeg를 찾을 수 없음 |

---

## 문서

| 문서 | 내용 |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | 설계, stream-copy 파이프라인, 상태 머신, 복구 |
| [TESTING.md](TESTING.md) | 테스트 전략과 실행 방법 |
| [BENCHMARK.md](BENCHMARK.md) | 실측 성능 수치와 측정 방법 |
| [LICENSES.md](LICENSES.md) | FFmpeg 및 서드파티 라이선스 |
| [IMPLEMENTATION_REPORT.md](IMPLEMENTATION_REPORT.md) | 구현 현황과 실제 테스트 결과 |
| [RELEASING.md](RELEASING.md) | 설치 파일을 만들어 배포하는 절차 |

## 지원 환경

| | 최소 | 비고 |
| --- | --- | --- |
| OS | Windows 10/11 x64, macOS 11+, Linux x86_64 | Mac은 Apple Silicon·Intel 모두. Linux는 `.deb`와 `.AppImage` |
| CPU | Intel i5급 또는 Apple Silicon | 아직 최소 사양을 실측 검증하지 않았습니다 |
| RAM | 8 GB | |
| 저장공간 | 영상 1시간당 약 4.6 GB (1080p30) | 720p30은 약 1.9 GB |
| 네트워크 | 1080p30 기준 안정적인 업로드 8 Mbps 이상 | 유선 권장 (송출 6 Mbps + 여유) |
