# YouTube 연동 설정과 테스트

**이 문서는 선택 기능에 대한 것입니다. 읽지 않아도 방송은 됩니다.**

기본 방송은 영상 추가 → 스트림 키 입력 → 방송 시작, 세 단계가 전부입니다.
Louver Live는 영상을 RTMPS + 스트림 키로 내보내며, 여기에는 Google 로그인도
YouTube Data API도 Google Cloud 설정도 들어가지 않습니다.

여기서 설명하는 YouTube API 연동은 방송 제목·설명·태그·공개범위를 앱에서 바꾸고
라이브 채팅에 메시지를 자동으로 보내는 데만 씁니다. 연결하지 않아도 방송은 100%
정상 동작하고, 연결한 뒤 OAuth나 API에서 오류가 나도 FFmpeg 송출은 멈추지
않습니다 — 제목·채팅 기능만 비활성화됩니다.

> **이 문서의 절차는 이 저장소에서 실행되지 않았습니다.** 실제 Google 계정과
> 실제 라이브 방송이 필요하기 때문입니다. 코드 경로는 가짜 YouTube 서버를 상대로
> 전부 검증했지만(§테스트 현황), 아래 1~7번은 Mac에서 직접 하셔야 하고 그 전까지
> **NOT TESTED**입니다.

---

## 0. 이 기능에 요금이 붙나요

**아니요.** 연결해도, 써도 요금이 발생하지 않습니다.

- Louver Live가 쓰는 Google 서비스는 YouTube Data API 하나이고, **무료 한도
  안에서만** 동작합니다.
- 이 API를 제공하는 Google Cloud 프로젝트는 Louver Live 측이 관리하며, 결제
  계정이 연결되어 있지 않습니다. 청구될 대상 자체가 없습니다.
- 하루 한도를 다 쓰면 앱에 이렇게 표시됩니다 — "무료 API 사용량이 소진되었습니다.
  고급 YouTube 기능은 다음 초기화 후 다시 사용할 수 있습니다. 영상 방송에는 영향을
  주지 않습니다." 제목 적용과 자동 채팅만 다음 초기화까지 쉬고,
  **방송 송출은 멈추지 않습니다.**
- 사용하시는 분이 Google Cloud 프로젝트를 만들거나, 카드를 등록하거나, 할당량을
  설정할 일은 없습니다.

## 1. YouTube 계정 연결

**설정 → YouTube 고급 기능 (선택) → [YouTube 계정 연결]**

브라우저가 열리면 Google 로그인 후 권한을 허용하면 끝입니다. 앱에 채널명이
표시되면 연결된 것입니다.

Google Cloud 콘솔을 열 필요도, Client ID를 입력할 필요도 없습니다. 연결
버튼이 "이 빌드에는 클라이언트가 포함되어 있지 않습니다"라고 하면 개발 빌드를
쓰고 계신 것입니다 — 정식 빌드를 만드는 방법은
[YOUTUBE_OAUTH_PRODUCTION.md](YOUTUBE_OAUTH_PRODUCTION.md)에 있습니다.

### 연결한 뒤

| 화면 | |
| --- | --- |
| 채널 | 연결된 채널 이름 |
| 상태 | 연결됨 |
| [계정 변경] | 다른 Google 계정으로 바꿉니다 |
| [연결 해제] | 토큰을 지웁니다 |

### 저장 위치

refresh token은 macOS 키체인(Windows는 자격 증명 관리자)에 들어갑니다.
SQLite에는 채널명과 Channel ID만 남고, 토큰은 로그에도 기록되지 않습니다.
접근 토큰은 메모리에만 두고 만료되면 다시 받습니다.

### 권한

`youtube.force-ssl` 하나만 요청합니다. 채널 읽기, 방송 정보 수정, 태그 수정,
라이브 채팅 쓰기가 이 하나로 됩니다.

## 3. 실제 테스트 절차

YouTube Studio에서 **비공개(Private)** 또는 **일부공개(Unlisted)** 라이브를 하나
만들어 두고 시작하세요. 결과는 `rc-results/youtube-test.md`에 적으면 됩니다.

| # | 할 일 | 통과 기준 |
| --- | --- | --- |
| 1 | 방송 설정에서 **제목** 입력 → 저장 → 지금 YouTube에 적용 | YouTube 라이브 제목이 바뀐다 |
| 2 | **설명** 입력 후 같은 방법으로 적용 | YouTube 설명이 바뀐다 |
| 3 | **태그** 3개 입력 후 적용 | YouTube Studio 영상 세부정보의 태그가 바뀐다. **제목·설명·카테고리가 그대로 남아 있어야 한다** |
| 4 | 자동 채팅 메시지 3개 등록 → 방송 시작 | 실제 라이브 채팅에 순서대로 올라온다 |
| 5 | 방송 **종료** | 채팅 봇이 즉시 멈춘다 (로그에 `CHAT_STOPPED`) |
| 6 | 다시 **시작** | 새 채팅 ID를 받아 봇이 다시 붙는다 (로그에 `CHAT_CONNECTED`) |
| 7 | 시작/종료를 5회 반복 | 이전 방송의 채팅에 메시지가 가지 않는다. 매번 새 broadcast id가 로그에 찍힌다 |

### 2026-09-20에 추가된 항목 (TEST 1~5)

실제 방송이 앱에 입력한 값이 아니라 YouTube 기본값 "Playlist"로 시작한 문제를
고친 뒤, 아래도 함께 확인해주세요.

| # | 할 일 | 통과 기준 |
| --- | --- | --- |
| 8 | 제목·설명·태그 6개·카테고리 Music·공개범위 Unlisted 저장 → **방송 시작** | 라이브가 **처음부터** 입력한 제목으로 시작한다. "Playlist"로 시작하면 FAIL |
| 9 | 대시보드의 **YouTube 방송 설정** 카드 | 5개 항목 모두 `적용 완료`. 하나라도 `적용 실패`면 그 옆에 YouTube의 현재 값이 표시된다 |
| 10 | YouTube Studio에서 영상 세부정보 확인 | 제목·설명·태그·카테고리·공개범위가 앱에 입력한 값과 **글자 그대로** 같다 |
| 11 | 예약 방송으로 같은 절차 | 수동 시작과 동일한 결과. 다른 코드 경로가 아니다 |
| 12 | 방송 종료 → 제목 변경 → 다시 시작 | 새 제목이 적용된다. 이전 제목/liveChatId/broadcast가 재사용되지 않는다 |
| 13 | 계정 연결을 끊고 방송 시작 | "방송 설정을 YouTube에 적용하지 못했습니다" 선택창이 뜬다. 조용히 시작하면 FAIL |
| 14 | 13번에서 **설정 없이 방송 시작** 선택 | 영상은 정상 송출된다 |

8번과 10번이 가장 중요합니다. 실제 방송 제목은 broadcast가 아니라 **영상(video)
리소스**에서 읽히고, 태그를 쓰는 호출이 그 영상의 snippet 전체를 덮어씁니다.
그래서 이 앱은 병합할 때 제목과 설명까지 함께 씁니다 — 그러지 않으면 방금 바꾼
제목이 예전 값으로 되돌아갑니다.

3번은 여전히 가장 중요합니다. 태그만 바꾸는 API 호출은 영상의 제목과 카테고리를 통째로
날릴 수 있어서, Louver Live는 **기존 값을 먼저 읽어 병합한 뒤** 씁니다. 직접
확인해보실 값이 바로 그것입니다.

### 2026-09-20에 추가된 항목 (TEST 15~19) — 예약 방송 준비 단계 진단

실제 Mac에서 예약 방송이 `LL-YOUTUBE-004`만 여섯 번 남기고 FFmpeg까지 가지
못한 문제를 조사하기 위해, 준비 과정의 **모든 단계**가 로그에 남도록 했습니다.
아래는 그 로그를 실제로 얻기 위한 절차입니다.

| # | 할 일 | 통과 기준 |
| --- | --- | --- |
| 15 | 수동 방송 시작 → `app.log` 확인 | `origin=manual` 이 붙은 `_START`/`_OK` 줄이 단계마다 있다 |
| 16 | 같은 세션에서 예약 방송 실행 → `app.log` 확인 | `origin=scheduled` 줄의 **순서가 15번과 같다**. 다르면 그 차이가 원인이다 |
| 17 | 예약 방송이 실패한 경우 | `_FAIL` 줄에 API 메서드·HTTP 상태·Google reason·Google 메시지가 모두 있다 |
| 18 | 화면 확인 | 실패 창 제목이 `YouTube에 연결하지 못했습니다`가 아니라 실패한 **단계 이름**이다 |
| 19 | 로그 전체 검색 | 액세스 토큰·리프레시 토큰·클라이언트 시크릿·스트림 키가 한 글자도 없다 |

#### 실제 Mac에서 다시 테스트하는 방법

```bash
# 1. 이전 로그를 치워 이번 실행만 보이게 합니다
mv ~/Library/Application\ Support/LouverLive/logs/app.log \
   ~/Library/Application\ Support/LouverLive/logs/app.log.before

# 2. 공식 OAuth 클라이언트를 넣고 빌드합니다 (둘 다 필요합니다)
export LOUVER_GOOGLE_CLIENT_ID="…apps.googleusercontent.com"
export LOUVER_GOOGLE_CLIENT_SECRET="GOCSPX-…"
npm run build

# 3. 앱을 실행하고
#    - 설정 → YouTube 계정 연결
#    - 방송 설정에 제목·설명·태그·카테고리·공개범위 저장
#    - 방송 예약에 "지금부터 2분 뒤 시작, 10분 뒤 종료" 예약을 만들고
#    - [예약 방송 시작] 을 누른 뒤 **아무것도 누르지 않고 기다립니다**

# 4. 예약 시각이 지난 뒤, 준비 과정을 순서대로 봅니다
grep -E "YOUTUBE_(TOKEN|BROADCAST|STREAM|METADATA)_" \
  ~/Library/Application\ Support/LouverLive/logs/app.log

# 5. 실패했다면 그 한 줄이 원인을 말해줍니다
grep "_FAIL" ~/Library/Application\ Support/LouverLive/logs/app.log

# 6. 비교: 같은 빌드에서 수동 방송도 한 번 돌린 뒤
grep -E "origin=(manual|scheduled)" \
  ~/Library/Application\ Support/LouverLive/logs/app.log

# 7. 자격 증명이 새지 않았는지 — 아무것도 나오면 안 됩니다
grep -rniE "ya29\.|1//[A-Za-z0-9_-]{20,}|GOCSPX|$(security find-generic-password \
  -s com.louver.live -a stream_key -w 2>/dev/null | cut -c1-6)" \
  ~/Library/Application\ Support/LouverLive/logs/
```

**PASS 기준은 하나뿐입니다.** 로그에 아래 순서가 모두 나오고, 실제 YouTube
채널에서 라이브가 보여야 합니다.

```
YOUTUBE_BROADCAST_CREATED  →  YOUTUBE_BROADCAST_BOUND
  →  FFmpeg Running  →  YOUTUBE_BROADCAST_LIVE
```

여기까지 가지 못하면 어떤 단계에서 멈췄든 **FAIL**입니다. 그리고 멈춘 단계는
이제 `_FAIL` 줄 하나로 알 수 있습니다.

### 확인용 로그

**로그** 페이지 또는 `~/Library/Application\ Support/LouverLive/logs/`:

```
YOUTUBE_AUTH_CONNECTED    계정 연결됨
YOUTUBE_METADATA_UPDATED  제목·설명·공개범위 적용됨
YOUTUBE_TAGS_UPDATED      태그 적용됨
CHAT_CONNECTED            라이브 채팅에 붙음
CHAT_MESSAGE_SENT         메시지 전송됨 (내용은 반복 기록하지 않음)
CHAT_MESSAGE_FAILED       전송 실패 + 오류 코드
YOUTUBE_METADATA_VERIFIED 적용 결과를 다시 읽어 확인함
YOUTUBE_METADATA_MISMATCH 반영되지 않은 항목이 있음
YOUTUBE_METADATA_SKIPPED  계정 미연결 등으로 적용하지 않음
CHAT_RETRY                재시도 예정
CHAT_STOPPED              봇 종료
```

준비 과정은 단계마다 `_START` / `_OK` / `_FAIL` 로 남습니다:

```
YOUTUBE_PREPARE_START            준비 시작 (origin=manual 또는 origin=scheduled)
YOUTUBE_TOKEN_REFRESH_*          Google 인증 갱신
YOUTUBE_BROADCAST_LIST_*         이 예약에 쓸 방송이 이미 있는지
YOUTUBE_BROADCAST_INSERT_*       없으면 새로 만듦
YOUTUBE_STREAM_LIST_*            스트림 키가 가리키는 수신 지점 찾기
YOUTUBE_BROADCAST_BIND_*         방송과 수신 지점 연결
YOUTUBE_METADATA_APPLY_*         제목·설명·태그·카테고리·공개범위
YOUTUBE_STREAM_ACTIVE_WAIT       YouTube가 아직 영상을 받지 못함
YOUTUBE_STREAM_ACTIVE            YouTube가 영상을 받기 시작함
YOUTUBE_BROADCAST_TRANSITION_*   LIVE 전환
YOUTUBE_PREPARE_FAIL             어느 단계에서 멈췄는지 + 오류 코드
```

`_FAIL` 줄에는 Google이 답한 그대로가 들어갑니다 — API 메서드, HTTP 상태,
Google의 error reason, Google의 메시지. 예를 들어
`liveBroadcasts.insert HTTP 403 reason=liveStreamingNotEnabled` 처럼 나옵니다.

토큰과 스트림 키는 어떤 로그에도 남지 않습니다.

---

## 4. 알아두실 점

**채팅은 방송이 실제 LIVE가 된 뒤에 붙습니다.** YouTube는 방송이 시작되기 전에는
`activeLiveChatId`를 주지 않습니다. 그동안 상태는 `WAITING_FOR_LIVE_CHAT`이고,
연결되면 `CONNECTED`로 바뀝니다.

**전송 간격은 최소 5분입니다.** UI에서도 그 아래를 고를 수 없고, 설정을 직접
고쳐도 엔진이 5분으로 올립니다. 몇 초 간격으로 반복 전송하면 채널이 스팸으로
취급될 수 있기 때문입니다.

**채팅이 실패해도 방송은 멈추지 않습니다.** 봇은 별도 스레드에서 돌고, 실패하면
봇만 멈추거나 1분 뒤 다시 시도합니다.

| 화면에 보이는 오류 | 뜻 | 봇의 행동 |
| --- | --- | --- |
| `LL-CHAT-001` | 너무 자주 보냄 | 1분 뒤 재시도 |
| `LL-CHAT-002` | 이 방송은 채팅이 꺼져 있음 | 중지 |
| `LL-CHAT-003` | 채팅이 종료됨 | 중지 |
| `LL-YOUTUBE-002` | 로그인 만료 | 중지 — 설정에서 다시 연결 |
| `LL-YOUTUBE-005` | 오늘 API 사용량 초과 | 중지 — 내일 자동 재개 |
| `LL-YOUTUBE-004` | 일시적 오류 | 1분 뒤 재시도 |

**API 사용량(quota).** 기본 할당량은 하루 10,000 units입니다. 채팅 메시지
한 건이 약 50 units이므로 20분 간격이면 하루 72건 ≈ 3,600 units입니다.
메타데이터 적용은 방송당 100 units 안쪽입니다. 24시간 방송에 20분 간격이면
충분하지만, 간격을 크게 줄이면 부족해질 수 있습니다.

---

## 5. 테스트 현황

결과는 `rc-results/youtube-test.md`에 적습니다. 그 파일이 채워지기 전까지 아래
표의 마지막 네 줄은 NOT TESTED입니다.

| 항목 | 상태 |
| --- | --- |
| 메타데이터 검증 (제목 100자, 태그 500자, 따옴표 비용) | **PASS** — 단위 테스트 |
| 태그 병합이 제목·설명·카테고리를 보존 | **PASS** — 가짜 YouTube 서버에 실제로 나간 요청 본문으로 확인 |
| YouTube 오류 → 사용자 메시지 매핑 | **PASS** — 5가지 실패 응답으로 확인 |
| 채팅 순서·간격·중복 방지·재시작 초기화 | **PASS** — 단위 테스트 |
| OAuth 토큰 저장·갱신·회전 | **PASS** — 단위 테스트 (Google 대신 가짜 엔드포인트) |
| UI 입력·태그 칩·프리셋·메시지 관리 | **PASS** — UI e2e 테스트 + 실제 앱에서 직접 조작 |
| **실제 Google 계정 OAuth** | **NOT TESTED** |
| **실제 YouTube 라이브 메타데이터 변경** | **NOT TESTED** |
| **실제 라이브 채팅 전송** | **NOT TESTED** |
| **Stop → Start 후 stale liveChatId 없음** | **NOT TESTED** |
| 준비 단계별 START/OK/FAIL 로그 | **PASS** — 단위·통합 테스트 (실패 줄에 메서드·상태·reason·메시지가 모두 담기는지) |
| 자격 증명이 로그에 남지 않음 | **PASS** — 네 가지 값 모두로 로그 전문을 검색하는 테스트 |
| **실제 Mac 예약 방송 준비 로그 (TEST 15~19)** | **NOT TESTED** |
| **실제 YouTube에서 LIVE 확인** | **NOT TESTED** |

아래 세 줄은 이 저장소에서 확인할 방법이 없습니다. 위 3번 절차를 Mac에서
돌려보시면 채워집니다.
