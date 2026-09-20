# YouTube OAuth — 제품 배포 준비

> **이 문서는 선택 기능(제목·설명·태그 자동 관리, 자동 라이브 채팅)을 위한
> 것입니다.** 기본 방송은 스트림 키만으로 동작하며 OAuth가 전혀 들어가지
> 않습니다. 이 절차를 하나도 밟지 않아도 제품은 방송 프로그램으로서 완전합니다.

고급 기능을 켜는 사용자는 **[YouTube 계정 연결]** 버튼 하나만 누릅니다. Google Cloud 콘솔은
열지 않습니다. 그렇게 만들려면 Louver Live 쪽에서 OAuth 클라이언트를 하나
만들어 빌드에 넣고, Google 검증을 받아야 합니다. 이 문서가 그 절차입니다.

> 이 저장소에는 실제 클라이언트가 들어 있지 않습니다. 아래 1~3번은 제품
> 소유자가 직접 하셔야 하고, 그 전까지 릴리스 빌드는 연결 버튼을 눌러도
> "이 빌드에는 클라이언트가 포함되어 있지 않습니다"라고 표시합니다.

---

## 0. 비용 원칙 — 먼저 읽을 것

**Louver Live의 YouTube 연동 때문에 Google Cloud 요금이 발생해서는 안 됩니다.**
이건 코드로 보장할 수 없는 부분이 있고, 프로젝트 설정으로 보장해야 합니다.

| 규칙 | 어떻게 지키는가 |
| --- | --- |
| 결제 계정 연결 금지 | 프로젝트에 **Billing Account를 연결하지 않습니다.** 연결하지 않으면 청구할 대상 자체가 없고, 할당량을 넘긴 요청은 과금이 아니라 **거부**됩니다 |
| 종량제(pay-as-you-go) 금지 | 결제 계정이 없으면 불가능합니다. 별도로 켤 것이 없습니다 |
| 할당량 상향 신청 금지 | "Quota increase request"를 넣지 않습니다. 무료 기본 한도로 운영합니다 |
| 유료 Cloud 기능 사용 금지 | 이 제품이 쓰는 Google 서비스는 **YouTube Data API v3 하나뿐**입니다. Cloud Storage·BigQuery·Logging 등 과금 대상 서비스는 활성화하지 않습니다 |

> 확인 방법: **결제 → 이 프로젝트에 연결된 결제 계정** 이 "없음"이어야 합니다.
> 연결되어 있다면 **결제 계정 연결 해제**를 누르세요.

### 한도를 다 썼을 때

무료 기본 한도는 프로젝트당 **하루 10,000 단위**입니다. 다 쓰면 그날은 API가
거부되고, 태평양 시간 자정에 초기화됩니다. 그때 앱의 동작:

| 계속 동작 | 쉬었다가 다음 날 재개 |
| --- | --- |
| Stream Key RTMPS 송출 | 제목·설명·태그·카테고리·공개범위 자동 적용 |
| 플레이리스트 | 자동 라이브 채팅 |
| 예약 스케줄러 | |
| 자동 재연결 | |

앱은 여기서 **방송을 멈추지 않습니다.** 대시보드에 "오늘 사용량 초과"로 표시하고,
송출은 그대로 이어갑니다.

### 한도에는 두 종류가 있습니다

| | 단위 | 초기화 |
| --- | --- | --- |
| **공용 풀 (combined units)** | 하루 10,000 단위. 읽기 1, 쓰기 50 | 태평양 시간 자정 |
| **메서드별 개별 버킷** | `search.list`, `videos.insert` — **하루 100회**, 호출당 1 단위 | 태평양 시간 자정 |

2026년 개편으로 `search.list`는 호출당 100 단위가 아니라 **하루 100회의 별도
버킷**이 되었습니다. 이걸 100 단위 쓰기로 계산하면 두 방향 모두 틀립니다 —
공용 풀에서 100배를 과다 차감하면서, 정작 실제로 막는 한도(호출 횟수)는 전혀
세지 않습니다. 그래서 두 한도를 따로 셉니다.

### 메서드별 단가표

`crates/louver-core/src/youtube/quota.rs`의 `COSTS`가 유일한 출처입니다.
코드 어디에도 다른 숫자가 없습니다.

| 메서드 | 단위 | 개별 버킷 | 이 앱이 사용 |
| --- | --- | --- | --- |
| `channels.list` | 1 | — | ○ (계정 연결 시 채널명) |
| `liveBroadcasts.list` | 1 | — | ○ |
| `liveBroadcasts.update` | 50 | — | ○ |
| `liveBroadcasts.insert` | 50 | — | ✕ |
| `liveBroadcasts.bind` | 50 | — | ✕ |
| `liveBroadcasts.transition` | 50 | — | ✕ |
| `liveStreams.insert` / `.update` | 50 | — | ✕ |
| `videos.list` | 1 | — | ○ |
| `videos.update` | 50 | — | ○ |
| `liveChatMessages.list` | 1 | — | ✕ |
| `liveChatMessages.insert` | 50 | — | ○ (자동 채팅) |
| `search.list` | 1 | **100회/일** | ✕ |
| `videos.insert` | 1 | **100회/일** | ✕ |
| (표에 없는 호출) | 50 | — | — |

> **출처와 한계.** 이 표는 제품 소유자가 Google 공식 quota calculator(2026)에서
> 확인해 전달한 값입니다. 이 저장소가 도는 환경에서는
> `developers.google.com`에 접근할 수 없어 직접 대조하지 못했습니다. 따라서
> 단일 출처이며, Google 쪽이 달라지면 **고칠 곳은 `COSTS` 한 군데**입니다.

개별 버킷을 쓰는 두 메서드는 이 제품이 호출하지 않습니다.
`the_app_calls_nothing_from_a_granular_bucket` 테스트가 그 상태를 고정하고 있어,
나중에 추가하려면 그 테스트를 의도적으로 고쳐야 합니다.

### 앱이 한도를 지키는 방식

- **계량이 전송 계층에 있습니다.** `MeteredClient`가 `HttpClient`를 감싸고, 앱에서
  YouTube Data API에 닿는 경로는 이것 하나뿐입니다. 호출부가 예산 확인을
  빠뜨릴 수 있는 구조가 아닙니다. 별도 스레드에서 도는 채팅 봇도 같은 계량기를
  씁니다.
- **모르는 호출은 쓰기(50 단위) 가격**으로 계산합니다. 낮게 잡으면 한도를 넘어
  403을 맞고, 높게 잡으면 일찍 멈춥니다. 안전한 쪽은 하나뿐입니다.
- **Google의 거부가 로컬 계산을 이깁니다.** `quotaExceeded`를 받으면 그 한도를
  즉시 닫습니다. 개별 버킷에서 받은 거부는 **그 버킷만** 닫고 공용 풀은
  건드리지 않습니다.
- 마지막 200 단위는 예비로 남깁니다. 메타데이터 적용 1회(103 단위)와 채팅
  1건(50 단위)이 들어가는 크기라, 제목을 바꾸는 도중이 아니라 앱이 스스로
  판단해서 멈춥니다.
- 사용량은 DB에 보존되어 재시작해도 이어서 셉니다.

실측 기준 하루 사용량: 방송 1회의 메타데이터 적용(읽기 3 + 쓰기 2 = 103 단위)과
20분 간격 채팅 72건(3,600 단위) = **3,704 단위**. 한도의 40% 미만입니다.
(`a_days_realistic_use_fits_comfortably`)

---

## 1. Google Cloud 프로젝트

1. <https://console.cloud.google.com/> → **프로젝트 만들기** → 이름 `Louver Live`
2. **API 및 서비스 → 라이브러리** → `YouTube Data API v3` → **사용**
3. **결제 계정은 연결하지 않습니다** (§0). 연결하라는 안내가 나와도 건너뜁니다 —
   YouTube Data API는 결제 계정 없이 무료 한도로 동작합니다.

> 이 세 단계는 **제품 소유자가 한 번만** 합니다. 앱을 쓰는 사람은 이 화면을
> 볼 일이 없습니다.

## 2. OAuth 동의 화면 (브랜딩)

**API 및 서비스 → OAuth 동의 화면**

| 항목 | 값 |
| --- | --- |
| User Type | 외부 |
| 앱 이름 | Louver Live |
| 사용자 지원 이메일 | 지원용 주소 |
| 앱 로고 | 120×120 PNG (선택, 검증 시 권장) |
| 애플리케이션 홈페이지 | **필수** — 공개 URL |
| 개인정보처리방침 URL | **필수** — 공개 URL |
| 서비스 약관 URL | **필수** — 공개 URL |
| 승인된 도메인 | 위 URL들의 도메인 |
| 개발자 연락처 | 이메일 |

홈페이지·개인정보처리방침·이용약관은 **실제로 접근 가능한 공개 페이지**여야
합니다. Google 검토자가 직접 엽니다. 개인정보처리방침에는 앱이 어떤 데이터를
어떻게 쓰는지 적어야 합니다 — Louver Live의 경우 "사용자의 YouTube 채널
정보를 읽고, 사용자가 입력한 방송 제목·설명·태그를 사용자의 방송에 적용하며,
사용자가 입력한 메시지를 사용자의 라이브 채팅에 보냅니다. 데이터를 외부로
전송하거나 판매하지 않습니다"가 사실에 맞는 설명입니다.

### 범위

`https://www.googleapis.com/auth/youtube.force-ssl` **하나만** 요청합니다.
채널 읽기, 방송 메타데이터 수정, 영상 태그 수정, 라이브 채팅 쓰기가 전부 이
하나로 됩니다.

## 3. OAuth 클라이언트 만들기

**사용자 인증 정보 → 사용자 인증 정보 만들기 → OAuth 클라이언트 ID**

- 애플리케이션 유형: **데스크톱 앱**
- 이름: `Louver Live Desktop`

클라이언트를 만들면 **클라이언트 ID와 클라이언트 보안 비밀** 두 값이 나옵니다.
**둘 다** 빌드에 넣습니다. 저장소에는 넣지 않습니다.

```bash
export LOUVER_GOOGLE_CLIENT_ID="000000-xxxx.apps.googleusercontent.com"
export LOUVER_GOOGLE_CLIENT_SECRET="GOCSPX-xxxxxxxxxxxxxxxx"
npm run build
```

Google이 이 Desktop 클라이언트의 토큰 교환에 secret을 요구하므로, secret 없이
빌드한 릴리스는 연결 버튼이 항상 실패합니다 (아래 "client_secret" 절).

> **주의 — 빌드에 박는 값은 빌드할 때 있어야 합니다.** `option_env!`는 rustc가
> 컴파일하는 시점의 환경을 읽고, cargo는 이 의존성을 모르므로 재빌드를
> 트리거하지 않습니다. 이미 빌드된 바이너리에 나중에 변수를 export해도
> **빌드에 박힌 값은 바뀌지 않습니다.** 값을 바꿨다면
> `cargo clean -p louver-core` 후 다시 빌드하세요. (실행 시점 환경변수는
> 별도로 우선 적용되므로, 개발 중에는 export 후 실행만으로도 동작합니다.)

CI에서는 저장소 시크릿 `LOUVER_GOOGLE_CLIENT_ID`와
`LOUVER_GOOGLE_CLIENT_SECRET`으로 주입합니다. `npm run secret-scan`이 이 값들이
작업 트리나 커밋 기록에 들어가면 빌드를 막습니다.

---

## client_secret — 실측으로 결론이 났습니다

이 문서는 두 번 바뀌었습니다. 처음에는 포럼 글을 근거로 "필수"라고 단정했다가
철회했고, 그 다음에는 "측정해서 정하자"며 **secret 없이 보내는 것을 기본값**으로
두었습니다. 2026-09-20에 실제 Mac에서 실제 Google이 답했습니다:

```
HTTP 400
invalid_request
client_secret is missing.
```

**그래서 이 Desktop 클라이언트의 토큰 교환에는 client_secret을 항상 함께
보냅니다.** PKCE는 그대로 씁니다 — secret은 PKCE를 대신하는 게 아니라 함께
보내는 것입니다.

| | 그때 | 지금 |
| --- | --- | --- |
| 기본 동작 | secret 없이 PKCE만 | **PKCE + client_secret** |
| fallback 스위치 | 개발자 모드에 있음, 기본 꺼짐 | **제거됨** (`youtube_allow_secret_fallback` 설정도 없어짐) |
| Production 빌드 | secret 없이 빌드 | **ID와 Secret 둘 다 주입** |

### 왜 설정해도 전송되지 않았는가 — 원인 두 가지

Mac에서 두 환경변수를 모두 export하고 실행했는데도 Google이 missing을 반환한
데에는 각각 단독으로도 이 증상을 만들어내는 원인이 두 개 있었습니다.

1. **`apply_secret_policy`가 secret을 지우고 있었습니다.** 위의 "측정하자"
   설계에 따라, fallback 스위치가 꺼져 있으면(기본값) 자격증명에서
   `client_secret`을 비운 뒤 교환에 넘겼습니다. 빌드에 secret이 있어도
   전송되지 않는 것이 **의도된 동작**이었습니다. 이 함수와 스위치를 삭제했습니다.

2. **`option_env!`는 빌드 시점에만 읽습니다.** `BUILT_IN_CLIENT_SECRET`은
   `option_env!`로 정의되어 있어, rustc가 이 crate를 **컴파일할 때**의 환경을
   읽습니다. 게다가 cargo는 이 매크로가 그 변수에 의존한다는 사실을 모르므로
   재빌드 트리거도 걸리지 않습니다. 즉 **변수를 export하고 기존 빌드를 실행하면
   아무 일도 일어나지 않습니다.** 이제 `ClientCredentials::resolve()`가
   **실행 시점 환경변수를 먼저** 읽고, 없을 때만 빌드에 박힌 값을 씁니다.
   배포 빌드는 환경이 비어 있으므로 박힌 값으로 내려갑니다.

### 시작 로그로 확인하기

앱을 실행하면 `app.log` 맨 앞에 두 줄이 찍힙니다. **값은 절대 찍지 않습니다.**

```
[INFO] OAuth Client ID: configured
[INFO] OAuth Client Secret: configured
```

`missing`이 보이면 그 변수가 이 프로세스에 도달하지 않은 것입니다. 이 두 줄은
"분명히 설정했는데 왜 missing이냐"는 물음을 한 번 보고 끝내기 위한 것입니다.

### 실패했을 때 보고할 것

연결이 실패하면 설정 → YouTube 고급 기능에 나오는 한 줄과 `app.log`의 해당 줄을
그대로 알려주세요. Google의 응답은 요약하지 않고 status·`error`·
`error_description`을 그대로 보존합니다.

```
YouTube 연결 실패: LL-YOUTUBE-002 · HTTP 400 · invalid_request: client_secret is missing.
```

토큰이나 비밀 값은 포함되지 않습니다.

## 4. 검증 (verification)

**`youtube.force-ssl`은 민감한(sensitive) 범위입니다.** 앱을 "게시됨
(프로덕션)" 상태로 두고 테스트 사용자가 아닌 사람이 쓰게 하려면 Google 검증을
받아야 합니다
([Sensitive scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/sensitive-scope-verification)).

### 검증 전: 테스트 모드

- **테스트 사용자**에 등록한 계정(최대 100개)은 검증 없이 바로 씁니다.
- **주의: refresh token이 7일 뒤 만료됩니다.** 앱에 `LL-YOUTUBE-002`가 뜨고
  다시 연결해야 합니다. 24시간 방송을 계속 돌릴 거라면 게시 상태로 넘어가야
  합니다.

### 검증 절차

1. OAuth 동의 화면에서 **앱 게시** → 상태가 "프로덕션"이 됩니다.
2. **OAuth Verification Center**에서 데이터 액세스 검증을 신청합니다.
3. Google이 요구하는 것:
   - 공개된 홈페이지·개인정보처리방침·이용약관
   - 앱 소유 도메인 확인 (Search Console)
   - **데모 영상** — 사용자가 로그인하는 화면부터, 요청한 범위를 어디에 쓰는지
     까지 보여줘야 합니다. Louver Live의 경우: 계정 연결 → 방송 설정에서
     제목·설명·태그 입력 → YouTube 방송에 반영되는 화면 → 자동 채팅 메시지가
     올라가는 화면.
   - 범위를 왜 써야 하는지 설명
4. 검토는 보통 며칠에서 몇 주 걸립니다. 그동안 테스트 사용자로는 계속 쓸 수
   있습니다.

### 검증 없이 가는 선택지

채널 소유자 본인만 쓴다면 테스트 사용자로 두고 7일마다 다시 연결해도 됩니다.
제품으로 배포한다면 검증을 받아야 합니다.

---

## 5. 구현 확인 항목

| 항목 | 상태 |
| --- | --- |
| PKCE S256을 Google이 지원함 | **확인됨** — discovery 문서를 직접 받아 확인 |
| PKCE S256, RFC 7636 테스트 벡터 일치 | **PASS** — 단위 테스트 |
| 기본 동작이 client_secret 없는 교환 | **PASS** — 실제로 나간 폼 본문으로 확인 |
| secret이 있을 때만 전송 | **PASS** — 같은 테스트 |
| 거부 응답을 status·error·error_description 그대로 기록 | **PASS** — 단위 테스트 |
| `code_verifier`가 브라우저 URL에 실리지 않음 | **PASS** — 단위 테스트 |
| 매 요청마다 새 verifier·state, OS 난수원 사용 | **PASS** — 단위 테스트 |
| 토큰 교환에 `code_verifier` 포함 | **PASS** — 단위 테스트 |
| 일반 사용자 UI에 Client ID/Secret 입력란 없음 | **PASS** — UI 테스트 |
| 자체 클라이언트는 개발자 모드에서만 | **PASS** — 서비스에서 거부 |
| refresh token은 키체인에만, DB·로그 없음 | **PASS** — `rc_security`, secret 스캐너 |
| 빌드에 클라이언트가 없으면 그렇게 표시 | **PASS** — 단위 테스트 + UI |
| 빌드 시 `LOUVER_GOOGLE_CLIENT_ID` 주입이 실제로 동작 | **PASS** — 주입 후 앱을 띄워 버튼이 활성화되는 것 확인 |
| Production 빌드에 ID와 Secret이 함께 주입됨 | **NOT TESTED** — 실제 클라이언트 생성 후 확인할 항목 |
| 브라우저를 열지 못하면 주소를 보여줌 | **PASS** — UI 테스트 |
| 브라우저 열기 권한이 Google 동의 화면 한 곳으로 제한됨 | **PASS** — `capabilities/default.json`, Tauri가 빌드 시 검증 |
| 메서드별 단가가 2026 표와 일치 | **PASS** — 단위 테스트 |
| `search.list`·`videos.insert`가 100 단위가 아니라 1 단위 + 별도 버킷 | **PASS** — 단위 테스트 |
| 개별 버킷이 단위가 아니라 호출 횟수로 소진됨 | **PASS** — 100회 후 버킷만 닫히고 공용 풀은 9,000 단위 이상 남음 |
| 버킷 거부가 공용 풀을 닫지 않음 | **PASS** — 단위 테스트 |
| 전체 URL과 경로의 단가가 동일 | **PASS** — 단위 테스트 (호스트가 붙으면 Unknown으로 빠지던 버그를 통합 테스트가 발견) |
| 하루 사용량이 무료 한도 안에서 계산됨 | **PASS** — 단위 테스트 (하루치 사용량 3,704 단위) |
| 한도에 닿기 전에 앱이 스스로 멈춤 | **PASS** — 통합 테스트, 소켓에 나가지 않는 것까지 확인 |
| Google의 `quotaExceeded`가 그날을 닫음 | **PASS** — 통합 테스트, 두 번째 호출이 시도되지 않음 |
| 500 등 다른 실패는 그날을 닫지 않음 | **PASS** — 통합 테스트 |
| 한도 소진 시 방송이 계속됨 | **PASS** — UI 테스트, LIVE 도달 확인 |
| 한도 소진 시 채팅 봇이 시작하지 않음 | **PASS** — tick에서 차단 |
| 사용량이 재시작을 넘어 유지됨 | **PASS** — DB에 보존, 복원 후 이어서 계산 |
| **결제 계정이 연결되어 있지 않음** | **NOT TESTED** — 실제 프로젝트 생성 후 콘솔에서 확인할 항목 |
| Google이 이 Desktop 클라이언트에 client_secret을 요구함 | **확인됨** — 실제 Mac, 실제 Google, HTTP 400 `invalid_request: client_secret is missing` |
| secret이 있으면 요청 본문에 실제로 포함됨 | **PASS** — 단위 테스트가 6개 필드를 직접 검사 |
| secret이 없으면 빈 값이 아니라 필드 자체가 없음 | **PASS** — 단위 테스트 |
| secret을 요구하는 엔드포인트 상대로 교환 성공 | **PASS** — 통합 테스트, Google의 거부/수락을 그대로 재현 |
| refresh 요청에도 secret이 포함됨 | **PASS** — 단위·통합 테스트 |
| 실행 시점 환경변수가 빌드에 박힌 값보다 우선 | **PASS** — 실제 앱, 같은 바이너리로 missing→configured 확인 |
| secret이 로그·DB에 남지 않음 | **PASS** — 실제 앱 실행 후 로그 3개와 settings 테이블 검사, 0건 |
| `{:?}`가 secret을 출력하지 않음 | **PASS** — 단위 테스트 (파생 Debug가 출력하던 것을 발견해 수동 구현으로 교체) |
| **실제 Google 계정으로 연결** | **NOT TESTED** — 클라이언트 생성 후 확인 |
| **Google 검증 통과** | **NOT TESTED** |

## 6. 출처

- [OAuth 2.0 for iOS & Desktop Apps](https://developers.google.com/identity/protocols/oauth2/native-app)
- [Sensitive scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/sensitive-scope-verification)
- [Google OpenID Connect discovery 문서](https://accounts.google.com/.well-known/openid-configuration) — 이 저장소에서 직접 받아 확인한 유일한 1차 자료
- [Desktop OAuth PKCE 관련 포럼 글](https://discuss.google.dev/t/desktop-oauth-pkce-exchange-returns-invalid-request-after-successful-loopback-callback-with-no-client-secret/390526) — **참고용. 공식 문서가 아니므로 근거로 쓰지 않습니다.**
- [OAuth 2.0 for Mobile & Desktop Apps — YouTube Data API](https://developers.google.com/youtube/v3/guides/auth/installed-apps)
