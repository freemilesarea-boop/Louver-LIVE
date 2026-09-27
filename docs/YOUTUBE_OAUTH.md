# YouTube 계정 연결 (OAuth + Live Streaming API)

247streams는 두 가지 방법으로 송출합니다.

| | 수동 RTMPS (`manual_rtmps`) | 연결된 계정 (`youtube_account`) |
|---|---|---|
| 스트림 키 | 사용자가 YouTube Studio에서 복사해 붙여넣음 | 247streams가 YouTube에 요청해서 받음 |
| 영상 전송 | 됨 | 됨 (완전히 같은 FFmpeg 경로) |
| 제목·설명·공개 범위 | **안 됨** — Studio에서 직접 관리 | 됨 (`liveBroadcasts.insert` / `.update`) |
| "실시간 시작" | 사용자가 직접 누름 | 자동 (`enableAutoStart`, 필요하면 `transition`) |
| 필요한 설정 | 없음 | 이 문서의 설정 |

**수동 RTMPS는 그대로 남아 있습니다.** 이 문서의 설정을 하지 않아도 지금 돌고 있는
방송은 아무 영향을 받지 않고, 계정 연결 버튼만 나타나지 않습니다.

---

## 1. Google Cloud 프로젝트와 API

1. <https://console.cloud.google.com/projectcreate> 에서 프로젝트를 만듭니다
   (이미 있으면 그대로 사용).
2. **API 사용 설정**: <https://console.cloud.google.com/apis/library/youtube.googleapis.com>
   에서 *YouTube Data API v3* 를 활성화합니다. Live Streaming API는 Data API v3의
   일부이므로 따로 켤 것이 없습니다.
3. 방송할 채널에서 **실시간 스트리밍이 활성화**되어 있어야 합니다
   (<https://www.youtube.com/live_dashboard> — 처음 활성화하면 최대 24시간
   걸립니다). 꺼져 있으면 API가 `liveStreamingNotEnabled` 로 거절합니다.

## 2. 동의 화면 (OAuth consent screen)

<https://console.cloud.google.com/apis/credentials/consent>

- **User type**: 조직 계정만 쓰면 *Internal*, 그 밖에는 *External*.
- **Scopes**: `https://www.googleapis.com/auth/youtube.force-ssl` 하나만
  추가합니다. 라이브 방송을 만들고 관리하는 데 필요한 범위이고, 247streams는
  이것 말고는 요청하지 않습니다.
- *External* + *Testing* 상태에서는 **Test users** 에 연결할 계정을 직접 넣어야
  합니다. 넣지 않으면 동의 화면에서 `access_denied` 가 됩니다.
- 서비스 계정은 쓰지 않습니다. 서비스 계정은 YouTube 채널을 가질 수 없어서
  `liveBroadcasts.insert` 가 실패합니다.

## 3. OAuth 클라이언트 (Web application)

<https://console.cloud.google.com/apis/credentials> → **Create credentials** →
**OAuth client ID** → **Application type: Web application**.

- **Authorized redirect URIs** 에 247streams의 콜백 주소를 넣습니다.
  **글자 하나까지 정확히** 같아야 합니다 (scheme, 포트, 마지막 슬래시 포함):

  ```
  운영:   https://live.example.com/api/youtube/oauth/callback
  개발:   http://localhost:8080/api/youtube/oauth/callback
  ```

  두 개를 모두 등록해 두어도 됩니다.
- 만들면 **Client ID** 와 **Client secret** 이 나옵니다. 다음 단계에서만 씁니다.

> **IP 주소로는 연결할 수 없습니다.** Google은 `http://1.2.3.4:8080/...` 같은 리디렉션
> URI를 받아주지 않습니다. 운영 서버가 아직 `http://<ip>:8080` 이라면
> 계정 연결만은 도메인 + HTTPS를 붙인 뒤에 하거나, 개발용 `localhost` 서버에서
> 하세요. 영상 송출 자체는 IP-only 배포에서도 그대로 동작합니다.

## 4. 서버 설정

`.env` (gitignore 되어 있습니다) 에 넣습니다. `.env.example` 에는 **이름만** 있고
값은 없습니다.

```dotenv
YOUTUBE_CLIENT_ID=1234567890-xxxxxxxx.apps.googleusercontent.com
YOUTUBE_CLIENT_SECRET=GOCSPX-xxxxxxxxxxxxxxxx
YOUTUBE_OAUTH_REDIRECT_URI=https://live.example.com/api/youtube/oauth/callback
```

`docker-compose.yml` 의 `louver` 서비스는 `.env` 를 `env_file` 로 읽으므로
컨테이너에 그대로 전달됩니다. `environment:` 블록에 다시 적지 **않는** 이유는
`docker compose config` 가 그 값을 화면에 출력하기 때문입니다.

`YOUTUBE_OAUTH_REDIRECT_URI` 를 생략하면
`http://localhost:8080/api/youtube/oauth/callback` 이 기본값입니다.

적용:

```sh
docker compose up -d --build
docker compose exec louver louver-server --diagnose | head -20
```

`--diagnose` 의 첫 줄에 설정 상태가 나옵니다. client secret은 절대 출력되지 않고
`[REDACTED]` 로만 표시됩니다.

```
YouTube  : client_id=설정됨 len=72 client_secret=[REDACTED] redirect_uri=https://live.example.com/api/youtube/oauth/callback
```

## 5. 연결 흐름

```
브라우저                     247streams 서버                     Google
   │  송출 대상 탭                    │                              │
   │  "YouTube 계정 연결" ──────────► │  GET /api/youtube/oauth/start │
   │                                  │  · state 를 만들어 저장        │
   │  ◄──── 동의 URL ────────────────│    (1회용, 15분)              │
   │                                  │                              │
   │ ─────────────────────────────────────────────► 동의 화면         │
   │  ◄──────────────────── ?code=…&state=… ──────────────────────── │
   │  GET /api/youtube/oauth/callback │                              │
   │                                  │  · state 확인 (1회용)         │
   │                                  │  · code → 토큰 교환 ────────► │
   │                                  │  · channels.list ───────────► │
   │                                  │  · refresh token 봉인 저장     │
   │  ◄──── 303 /?youtube=connected ──│                              │
```

방송을 만들 때 (`provider = youtube_account`):

1. `liveBroadcasts.insert` — 제목·설명·공개 범위·`scheduledStartTime`
2. `liveStreams.insert` — 이 방송만의 ingestion 주소와 `streamName`
3. `liveBroadcasts.bind` — 둘을 연결하고 `boundStreamId` 로 확인
4. `streamName` 을 기존 stream key 저장소에 봉인하고, `rtmps` 주소를
   `stream_destinations` 의 한 행으로 저장

그 다음부터는 **손으로 넣은 키와 완전히 같은 경로**입니다. FFmpeg, watchdog,
플레이리스트, 복구 코드는 한 줄도 다르게 동작하지 않습니다.

시작하면:

- `liveStreams.list` 로 신호가 들어왔는지 확인 (무한 폴링이 아니라 10초 간격,
  최대 30회)
- `enableAutoStart` 가 켜져 있으면 YouTube가 알아서 라이브로 바꿉니다
- 꺼져 있으면 신호가 확인된 뒤에만 `liveBroadcasts.transition` 을 호출합니다
  (신호 없이 부르면 YouTube가 권한 오류처럼 보이는 실패를 돌려줍니다)

중지하면 `desired_state=stopped` → FFmpeg 종료 → 라이브였다면
`transition(complete)`.

## 6. 토큰은 어디에 있습니까

| 값 | 어디에 | 로그에 |
|---|---|---|
| client ID | 환경변수만 | 이름과 길이만 |
| client secret | 환경변수만. **DB에 저장하지 않습니다** | `[REDACTED]` |
| refresh token | `credentials` 테이블에 master key로 봉인 (`youtube:<id>:refresh`) | `[REDACTED len=N]` |
| access token | 같은 저장소에 캐시 (`youtube:<id>:access`) | `[REDACTED]` |
| `streamName` (스트림 키) | 기존 destination 키와 같은 저장소 (`destination:<id>`) | `[REDACTED len=N sha256:8자]` |
| authorization code | 어디에도 저장하지 않습니다 | 출력하지 않습니다 |

- API 응답에는 토큰을 담을 **필드 자체가 없습니다** (`YoutubeAccount` 참고).
  `apps/server/tests/http_api.rs` 가 이것을 검사합니다.
- master key를 바꾸면 저장된 refresh token도 읽을 수 없게 되므로 계정을 다시
  연결해야 합니다. 스트림 키와 같은 규칙입니다.
- Google이 refresh 응답에 refresh token을 **주지 않는 것이 정상**입니다. 그 경우
  기존 값을 그대로 둡니다 — 덮어쓰면 계정이 영구히 끊깁니다.

## 7. 문제 해결

### `redirect_uri_mismatch`
Google에 등록한 URI와 247streams가 보낸 URI가 다릅니다. 보낸 값은
`louver-server --diagnose` 의 `redirect_uri=` 에 그대로 나옵니다. `http`/`https`,
포트, 마지막 슬래시까지 같아야 합니다. IP 주소는 Google이 아예 받지 않습니다.

### `access_denied`
동의 화면에서 취소했거나, *External* + *Testing* 상태인데 그 계정이
**Test users** 에 없습니다.

### `연결 요청이 만료되었거나 이미 사용되었습니다`
`state` 는 1회용이고 15분 뒤에 만료됩니다. 어제 열어둔 탭에서 돌아왔거나
뒤로 가기로 콜백을 다시 열면 이 메시지가 나옵니다. 다시 연결하면 됩니다.

### `invalid_grant`
refresh token이 더 이상 유효하지 않습니다 — 사용자가 Google 계정 설정에서 앱
접근을 취소했거나, client를 다른 것으로 바꿨거나, 계정 비밀번호가 바뀌었습니다.
연결을 해제하고 다시 연결하세요.

### `Google이 refresh token을 주지 않았습니다`
`prompt=consent` 없이 재연결하면 이미 동의한 계정에는 refresh token이 오지
않습니다. 247streams는 항상 `prompt=consent` 를 붙이므로 이 메시지가 나오면
Google 쪽 상태를 의심하고 다시 시도하세요.

### `insufficientPermissions`
scope가 부족합니다. 동의 화면에 `youtube.force-ssl` 이 있는지 확인하고, 계정을
연결 해제한 뒤 다시 연결해 새 scope에 동의하세요.

### `liveStreamingNotEnabled`
그 채널에 실시간 스트리밍이 켜져 있지 않습니다.
<https://www.youtube.com/live_dashboard> 에서 활성화합니다. 처음이면 최대 24시간
걸립니다.

### `quotaExceeded` / `dailyLimitExceeded`
하루 API 할당량(기본 10,000 units)을 다 썼습니다. 방송 하나를 만드는 데
`liveBroadcasts.insert`(50) + `liveStreams.insert`(50) + `bind`(50) 가 들고,
상태 확인은 1회당 1 unit입니다. 늘리려면 Google Cloud 콘솔에서 할당량 증설을
신청해야 합니다.

### 대시보드는 "송출 중"인데 채널에 안 보입니다
`YouTube 연결 대기` 배지가 붙어 있다면 YouTube가 아직 신호를 받지 못한
것입니다 (보통 10–30초). 계속 그렇다면:

```sh
docker compose exec louver louver-server --diagnose
```

`YouTube :` 줄의 `broadcast=`, `stream=`, `status=` 와 `대상 :` 줄의 `host=`,
`스트림 키:` 지문을 확인하세요. 스트림 키는 출력되지 않습니다.

## 8. 실제 YouTube 검증 상태

이 저장소의 테스트는 **가짜 Google**을 상대로 돕니다
(`crates/louver-cloud/tests/youtube_provider.rs`). CI에서 `googleapis.com` 으로
나가는 요청은 하나도 없습니다.

**REAL YOUTUBE NOT VERIFIED** — 실제 Google credential과 실제 채널 동의로
확인한 항목은 없습니다. 위 흐름을 실제로 검증하려면 이 문서의 설정을 끝낸 뒤
직접 계정을 연결하고, 방송을 하나 만들어 YouTube Studio에서 확인해야 합니다.
