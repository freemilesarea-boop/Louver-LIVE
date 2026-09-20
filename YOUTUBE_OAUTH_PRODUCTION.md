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

## 1. Google Cloud 프로젝트

1. <https://console.cloud.google.com/> → **프로젝트 만들기** → 이름 `Louver Live`
2. **API 및 서비스 → 라이브러리** → `YouTube Data API v3` → **사용**

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

**클라이언트 ID를 빌드에 넣습니다. 저장소에는 넣지 않습니다.**

```bash
export LOUVER_GOOGLE_CLIENT_ID="000000-xxxx.apps.googleusercontent.com"
npm run build
```

`LOUVER_GOOGLE_CLIENT_SECRET`은 **넣지 않는 것이 기본**입니다 — 아래
"client_secret에 대하여"를 보세요. Google이 secret 없는 교환을 거부한다는 실제
증거가 나온 뒤에만 넣습니다.

CI에서는 저장소 시크릿 `LOUVER_GOOGLE_CLIENT_ID`로 주입합니다.
`npm run secret-scan`이 이 값들이 작업 트리나 커밋 기록에 들어가면 빌드를
막습니다.

---

## client_secret에 대하여 — 그리고 이전 문서의 정정

**이전 판에서 "Google 데스크톱 클라이언트는 PKCE를 써도 client_secret이 반드시
필요하다"고 단정했습니다. 그 근거는 공식 문서가 아니라 포럼 글이었습니다.
단정을 철회합니다.**

### 확인한 것 (Google 공식 소스)

Google의 OpenID Connect discovery 문서를 직접 받아 확인했습니다
(<https://accounts.google.com/.well-known/openid-configuration>):

```json
"token_endpoint": "https://oauth2.googleapis.com/token",
"token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
"code_challenge_methods_supported": ["plain", "S256"]
```

- **PKCE S256은 공식적으로 지원됩니다.**
- 토큰 엔드포인트가 광고하는 인증 방식은 `client_secret_post`와
  `client_secret_basic` 둘뿐이고, 공개 클라이언트(비밀 없음)를 뜻하는 `none`은
  **목록에 없습니다.**

### 확인하지 못한 것

`none`이 없다는 사실은 시사적이지만 **Desktop 클라이언트에 대한 결론은
아닙니다.** discovery 문서는 OpenID Connect 용도로 광고되는 값이고, 설치형 앱의
토큰 교환에서 `client_secret`이 필수인지 선택인지는 여기서 단정할 수 없습니다.
`developers.google.com`은 이 저장소의 네트워크에서 차단되어 있어 공식
네이티브 앱 문서를 직접 읽지 못했습니다.

### 그래서 이렇게 설계했습니다 — 주장 대신 측정

기본 동작은 **client_secret 없이 PKCE만으로 토큰을 교환하는 것**입니다.

- 빌드에 secret이 없으면 `client_secret` 파라미터를 **아예 보내지 않습니다**
  (빈 값으로 보내지 않습니다).
- Google이 거부하면, 응답을 **그대로** 기록합니다: HTTP status, `error`,
  `error_description`. 요약하지 않습니다 — `invalid_request`와
  `invalid_client`는 다른 이야기이기 때문입니다.
- 기록된 내용은 설정 화면과 `app.log`에 남고, `youtube_last_auth_diagnostic`
  설정에도 보관됩니다.
- fallback(`client_secret`을 함께 보내기)은 **기본값이 꺼져 있고**, 위 근거를
  보고 판단한 뒤에만 켭니다.

따라서 **3번 절차대로 secret 없이 성공하면, Production 빌드에 client_secret을
넣지 않습니다.** `LOUVER_GOOGLE_CLIENT_SECRET` 없이 빌드하면 됩니다.

### 실패했을 때 보고할 것

연결이 실패하면 설정 → YouTube 고급 기능에 나오는 한 줄과 `app.log`의 해당 줄을 그대로
알려주세요. 이런 모양입니다.

```
YouTube 연결 실패: LL-YOUTUBE-002 · HTTP 400 · invalid_request: client_secret is missing.
```

이 한 줄이 fallback이 필요한지 아닌지를 결정합니다. 토큰이나 비밀 값은 포함되지
않습니다.

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
| 브라우저를 열지 못하면 주소를 보여줌 | **PASS** — UI 테스트 |
| 브라우저 열기 권한이 Google 동의 화면 한 곳으로 제한됨 | **PASS** — `capabilities/default.json`, Tauri가 빌드 시 검증 |
| **client_secret 없이 실제 Google 토큰 교환 성공 여부** | **NOT TESTED** — Mac에서 확인할 핵심 항목 |
| **실제 Google 계정으로 연결** | **NOT TESTED** — 클라이언트 생성 후 확인 |
| **Google 검증 통과** | **NOT TESTED** |

## 6. 출처

- [OAuth 2.0 for iOS & Desktop Apps](https://developers.google.com/identity/protocols/oauth2/native-app)
- [Sensitive scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/sensitive-scope-verification)
- [Google OpenID Connect discovery 문서](https://accounts.google.com/.well-known/openid-configuration) — 이 저장소에서 직접 받아 확인한 유일한 1차 자료
- [Desktop OAuth PKCE 관련 포럼 글](https://discuss.google.dev/t/desktop-oauth-pkce-exchange-returns-invalid-request-after-successful-loopback-callback-with-no-client-secret/390526) — **참고용. 공식 문서가 아니므로 근거로 쓰지 않습니다.**
- [OAuth 2.0 for Mobile & Desktop Apps — YouTube Data API](https://developers.google.com/youtube/v3/guides/auth/installed-apps)
