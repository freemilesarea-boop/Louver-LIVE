# YouTube OAuth — 제품 배포 준비

사용자는 **[YouTube 계정 연결]** 버튼 하나만 누릅니다. Google Cloud 콘솔은
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

**클라이언트 ID와 보안 비밀을 빌드에 넣습니다. 저장소에는 넣지 않습니다.**

```bash
export LOUVER_GOOGLE_CLIENT_ID="000000-xxxx.apps.googleusercontent.com"
export LOUVER_GOOGLE_CLIENT_SECRET="GOCSPX-..."
npm run build
```

CI에서는 저장소 시크릿 `LOUVER_GOOGLE_CLIENT_ID` / `LOUVER_GOOGLE_CLIENT_SECRET`
으로 넣습니다. `npm run secret-scan`이 이 값들이 작업 트리나 커밋 기록에
들어가면 빌드를 막습니다.

---

## 왜 client_secret이 아직 필요한가

PKCE를 쓰면 공개 클라이언트는 보통 secret 없이 토큰을 교환할 수 있습니다.
**Google의 데스크톱 앱 클라이언트는 예외**로, PKCE를 써도 토큰 교환 요청에
`client_secret`을 요구합니다
([Google Developer forums](https://discuss.google.dev/t/desktop-oauth-pkce-exchange-returns-invalid-request-after-successful-loopback-callback-with-no-client-secret/390526),
[OAuth 2.0 for iOS & Desktop Apps](https://developers.google.com/identity/protocols/oauth2/native-app)).

그래서 Louver Live는 **둘 다** 씁니다.

- **PKCE (S256)** — 가로챈 authorization code를 다른 프로그램이 교환하지
  못하게 막습니다. 매 요청마다 새 `code_verifier`를 만들고, 브라우저로 나가는
  URL에는 해시(`code_challenge`)만 실립니다. RFC 7636 부록 B의 테스트 벡터로
  구현을 검증합니다.
- **client_secret** — Google이 요구하므로 빌드에 포함합니다. 데스크톱 앱에
  배포되는 값이라 그 자체로는 아무것도 지켜주지 못하며, Google도 설치형 앱의
  secret을 기밀로 보지 않습니다. **사용자에게는 절대 입력시키지 않습니다.**

`state`와 `code_verifier`는 OS 난수원에서 만듭니다.

---

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
| PKCE S256, RFC 7636 테스트 벡터 일치 | **PASS** — 단위 테스트 |
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
| **실제 Google 계정으로 연결** | **NOT TESTED** — 클라이언트 생성 후 확인 |
| **Google 검증 통과** | **NOT TESTED** |

## 6. 출처

- [OAuth 2.0 for iOS & Desktop Apps](https://developers.google.com/identity/protocols/oauth2/native-app)
- [Sensitive scope verification](https://developers.google.com/identity/protocols/oauth2/production-readiness/sensitive-scope-verification)
- [Desktop OAuth PKCE exchange requires a client secret (Google Developer forums)](https://discuss.google.dev/t/desktop-oauth-pkce-exchange-returns-invalid-request-after-successful-loopback-callback-with-no-client-secret/390526)
- [OAuth 2.0 for Mobile & Desktop Apps — YouTube Data API](https://developers.google.com/youtube/v3/guides/auth/installed-apps)
