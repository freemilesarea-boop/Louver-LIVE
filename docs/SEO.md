# 검색 노출 (SEO) — Phase 1

247streams.kr을 "수강생이 로그인하는 도구"가 아니라 "검색으로 발견되는 공개
서비스"로 만들기 위한 첫 단계입니다. 이 문서는 **무엇을 어떻게 구현했는지**,
**왜 그 방법을 골랐는지**, 그리고 **배포 후 사람이 해야 하는 일**을 적습니다.

## 1. 출발점

작업 전 상태는 다음과 같았습니다.

| 항목 | 상태 |
| --- | --- |
| `<title>` | 모든 URL이 `247streams` 하나 |
| meta description / canonical / robots | 없음 |
| Open Graph / Twitter / JSON-LD | 없음 |
| `robots.txt` / `sitemap.xml` | 없음 |
| 공개 페이지 | 없음 — 로그인하지 않은 방문자는 `/terms`, `/privacy`를 빼면 전부 로그인 화면 |

즉 Google이 색인할 수 있는 것은 "로그인 폼 한 장"뿐이었습니다. 서비스가 무엇을
하는지, 얼마인지, 어떤 문제를 해결하는지가 HTML 어디에도 없었습니다.

## 2. 고른 방법: 빌드 시점 정적 prerender

Vite SPA는 모든 URL에 같은 `index.html`을 돌려주고, 내용은 스크립트가 실행된
뒤에 생깁니다. 선택지를 비교하면:

| 방법 | 결과 | 비용 |
| --- | --- | --- |
| meta tag만 추가 | 모든 URL이 여전히 같은 title·description | 거의 없음 |
| 런타임 SSR (Node 서버 추가) | 완전하지만 배포 구조가 바뀜 | 서버 한 대 추가, 운영 복잡도 상승 |
| Next.js 등으로 재작성 | 완전하지만 앱 전체를 다시 씀 | 매우 큼 — 금지 범위 |
| **빌드 시점 prerender** | 공개 URL마다 자신의 HTML | vite 설정 1개, 스크립트 2개 |

마지막 방법을 골랐습니다. 서버 코드, 라우팅, 배포 방식은 그대로입니다.

```
npm run build:cloud
  ├─ tsc --noEmit -p apps/web/tsconfig.json
  ├─ vite build --config vite.web.config.ts     # 기존 그대로 → apps/web/dist
  └─ npm run seo:prerender
       ├─ vite build --config vite.seo.config.ts   # → apps/web/.seo-ssr/ (배포 안 함)
       └─ node scripts/seo-prerender.mjs           # → dist/<route>/index.html, robots.txt, sitemap.xml
```

`apps/server/src/lib.rs`의 정적 서빙은 이미
`ServeDir::new(dir).fallback(ServeFile::new(index))`이므로, `dist/pricing/index.html`이
있으면 `/pricing/`에 그 파일이 그대로 나갑니다. **서버는 한 줄도 바뀌지 않았습니다.**

### 한 벌의 내용, 두 번의 렌더

| 파일 | 역할 |
| --- | --- |
| `apps/web/src/seo/content.ts` | 모든 문장·가격·FAQ. 유일한 출처 |
| `apps/web/src/seo/Marketing.tsx` | 그 내용을 그리는 React 컴포넌트 |
| `apps/web/src/seo/head.ts` | title·description·canonical·robots·OG·JSON-LD, `robots.txt`, `sitemap.xml` |
| `apps/web/src/seo/prerender-entry.tsx` | 빌드 시점에 Node가 호출하는 진입점 |
| `scripts/seo-prerender.mjs` | 결과를 `dist`에 쓰는 스크립트 |

같은 컴포넌트를 빌드 시점(정적 HTML)과 브라우저(`App.tsx`)가 모두 렌더합니다.
따라서 크롤러가 받는 HTML과 사람이 보는 화면이 서로 다를 수 없습니다 — 한 군데서
문장을 고치면 둘 다 바뀝니다.

예외는 하나입니다: `/`의 로그인 카드는 transport가 필요하므로 브라우저만
렌더합니다. 정적 HTML에는 같은 마케팅 본문이 폼 없이 들어갑니다.

## 3. 공개 URL

| URL | 검색 의도 | 비고 |
| --- | --- | --- |
| `/` | "247streams가 무엇인가" | 로그인·회원가입 폼이 **그대로 이 페이지에** 있습니다 |
| `/youtube-24-live/` | "유튜브 24시간 라이브를 PC 없이 유지하는 방법" | 문제(절전·재부팅·전기요금) → 해결 |
| `/playlist-live/` | "여러 영상을 이어 붙여 하나의 라이브로" | 플레이리스트 구성·제약 |
| `/youtube-live-streaming/` | "라이브 스트리밍을 서버에서 실행" | 송출 대상 등록, 되는 것과 안 되는 것 |
| `/pricing/` | "얼마인가" | 가격은 `SEED_PLANS`와 테스트로 묶여 있습니다 |
| `/terms/`, `/privacy/` | 법적 문서 | 기존 문구 그대로, 자신의 title·canonical을 가짐 |

페이지마다 FAQ 질문이 겹치지 않습니다. 같은 내용을 키워드만 바꿔 여러 URL에
복제하는 doorway page는 만들지 않았습니다.

### canonical과 trailing slash

`ServeDir`는 `/youtube-24-live`를 `/youtube-24-live/`로 **307 리다이렉트**하고
디렉터리 쪽에 파일을 서빙합니다. 그래서 canonical·sitemap·내부 링크는 모두
슬래시가 붙은 형태를 씁니다 — 우리 링크나 sitemap을 따라오는 크롤러는
리다이렉트를 한 번도 만나지 않습니다. `/`만 예외로 `https://247streams.kr/`입니다.

canonical은 항상 절대 URL이고 항상 `https://247streams.kr` 기준이므로, 쿼리
스트링(`?utm_source=…`)이 붙어도 색인은 한 URL로 모입니다.

## 4. 구조화 데이터

`@graph` 하나에 다음을 담습니다.

| 타입 | 어디에 | 내용 |
| --- | --- | --- |
| `Organization`, `WebSite` | 모든 공개 페이지 | 서비스 이름·URL·설명 |
| `SoftwareApplication` | `/`, `/pricing/` | `offers`가 `PUBLIC_PLANS`에서 생성 — 가격이 페이지와 동일 |
| `FAQPage` | FAQ가 있는 페이지 | 질문·답변이 **화면에 보이는 것과 글자까지 같음** |
| `BreadcrumbList` | `/` 이외 | 홈 → 현재 페이지 |

**`AggregateRating`, `Review`는 넣지 않았습니다.** 리뷰가 존재하지 않기 때문이고,
없는 평점을 만들어 넣는 것은 구조화 데이터 정책 위반입니다. 검사 스크립트가
`aggregateRating`/`ratingValue`/`review`가 HTML에 나타나면 빌드를 실패시킵니다.

`og:image`는 넣지 않았습니다. 공유 이미지가 아직 없고, 없는 이미지를 가리키는
태그는 빈 카드를 만듭니다. Phase 2 항목입니다.

## 5. robots.txt / sitemap.xml

```
User-agent: *
Allow: /
Disallow: /admin
Disallow: /billing/complete
Disallow: /api/

Sitemap: https://247streams.kr/sitemap.xml
```

`Disallow`가 보안 장치는 아닙니다 — 권한은 서버가 요청마다 다시 확인하고,
`/admin`은 크롤러에게 막혀 있든 아니든 회원에게 거부됩니다. 이 줄의 역할은
비공개 화면의 로그아웃 상태 렌더가 색인되지 않게 하고, 크롤 예산을 읽을 것이 있는
페이지에 쓰게 하는 것입니다.

`sitemap.xml`은 `<loc>`만 넣습니다. `lastmod`는 실제 내용 변경 시각이어야 하는데
빌드는 그것을 모르고, 배포마다 현재 시각을 찍는 것은 검색엔진이 곧 무시하게 되는
거짓말입니다. `changefreq`/`priority`는 Google이 쓰지 않는다고 밝혔습니다.

## 6. 없는 페이지 (soft 404)

서버는 파일이 없는 모든 경로에 앱 셸과 **200**을 돌려줍니다. 그래야 로그인한
사용자의 `/admin/users` 같은 딥링크가 동작합니다. 이 동작은 바꾸지 않았습니다.

대신 프런트엔드가 사실을 말합니다 (`apps/web/src/seo/robots-meta.ts`):

- 알 수 없는 경로는 "페이지를 찾을 수 없습니다" 화면을 렌더하고,
- `<meta name="robots" content="noindex,follow">`를 넣고,
- 서버가 보낸 **canonical을 제거합니다** — `noindex`를 `/`를 가리키는 canonical과
  함께 두면 그 `noindex`가 `/`로 옮겨갈 수 있습니다.

이것은 완화책이며 404가 아닙니다. 상태 코드는 여전히 200이고, 프런트엔드는 그것을
바꿀 수 없습니다. 진짜 404를 돌려주려면 서버의 fallback을 고쳐야 하는데, 그것은
이번 범위(서버 변경 금지)를 벗어납니다 — Phase 2 후보입니다.

## 7. 과장하지 않기

구현되지 않은 것, 확인할 수 없는 것은 쓰지 않습니다. `scripts/seo-check.mjs`가
빌드된 HTML에서 다음 표현을 찾으면 실패합니다.

`100%` · `무조건` · `절대 끊기지` · `끊기지 않습니다` · `보장합니다/됩니다` ·
`완벽한/하게` · `업계 최고/1위` · `최고의` · `수익을 보장` · `조회수가 오른다` ·
`구독자가 늘어난다` · `무중단` · `장애 없음`

반대로 "중단이 전혀 일어나지 않는다고 보장하지는 않습니다"처럼 한계를 밝히는
문장은 그대로 둡니다. 패턴은 단어가 아니라 **주장**을 찾습니다.

키워드 스터핑도 검사합니다: 본문(헤더·푸터 제외) 글자 수에서 한 키워드가
차지하는 비율이 6%를 넘으면 실패합니다. 현재 최대값은 2.95%입니다.

## 8. 자동 검증

| 명령 | 무엇을 보는가 |
| --- | --- |
| `npx vitest run apps/web/src/seo/seo.test.tsx` | 내용·head·robots/sitemap 생성기, 렌더된 페이지, `App.tsx` 라우팅 (30 테스트) |
| `npx vitest run --config vitest.tooling.config.ts scripts/seo-check.test.mjs` | 검사 스크립트 자체 — 일부러 망가뜨린 `dist`를 잡아내는지 (12 테스트) |
| `npm run seo:check` | **배포될 실제 HTML** — `npm run verify`에 포함 |
| `node scripts/release-smoke.mjs` (step 21) | 실제 서버가 서빙하는 응답: 7페이지의 title·canonical·본문·h1, 307, robots.txt, sitemap.xml |
| `node scripts/mobile-smoke.mjs` | 360 / 375 / 393 / 768 / 1280 px에서 가로 스크롤·화면 밖 컨트롤·h1 개수 |

가격은 코드에서 `crates/louver-cloud/src/db.rs`의 `SEED_PLANS`를 **파싱해서**
비교합니다 (`scripts/seo-plan-source.mjs`). 서버에서 가격을 바꾸고 공개 페이지를
고치지 않으면 테스트가 깨집니다. 공개 페이지에 가격을 적는 이유는 빌드 시점에는
물어볼 API가 없기 때문이고, 그 위험을 이 비교가 막습니다.

## 9. 배포 후 사람이 할 일

코드가 할 수 없는 일입니다. Search Console 사이트 등록 자체는 이미 완료되어
있으므로, 아래는 그 다음 단계입니다.

1. **배포 확인** — `https://247streams.kr/robots.txt`와 `/sitemap.xml`이 열리는지,
   `https://247streams.kr/youtube-24-live/`가 200으로 열리고 페이지 소스에
   `<title>`과 `<link rel="canonical">`이 보이는지 (JavaScript를 끈 상태로 보면
   prerender가 제대로 되었는지 바로 알 수 있습니다).
2. **sitemap 제출** — Search Console → 색인 → Sitemaps → `sitemap.xml` 추가.
3. **URL 검사** — 7개 공개 URL을 각각 "URL 검사"에 넣고,
   - "Google에 등록됨" 또는 "URL이 Google에 등록되어 있지 않음"을 확인,
   - **렌더링된 HTML**과 **스크린샷**을 열어 본문이 보이는지 확인,
   - 사용자가 선택한 canonical과 Google이 선택한 canonical이 같은지 확인,
   - "색인 생성 요청"을 누릅니다.
4. **리치 결과 테스트** — https://search.google.com/test/rich-results 에
   `/pricing/`과 `/youtube-24-live/`를 넣어 `FAQPage`·`SoftwareApplication`이
   오류 없이 인식되는지 확인.
5. **모바일 사용성** — Search Console → 환경 → 모바일 사용성에 새 URL이 오류 없이
   들어오는지 확인 (보고까지 며칠 걸립니다).
6. **1~2주 후** — 성능 보고서에서 노출·클릭·평균 게재순위가 잡히는 쿼리를 확인.
   의도와 다른 쿼리로만 노출되면 그 페이지의 title·description·H2를 그 의도에
   맞게 고치는 것이 Phase 2의 출발점입니다.
7. **색인 제외 확인** — 색인 → 페이지 보고서에서 `/admin`, `/billing/complete`,
   존재하지 않는 URL이 "색인 생성됨"에 올라오지 않는지 확인.

## 10. Phase 1에 넣지 않은 것

| 항목 | 이유 |
| --- | --- |
| 진짜 404 상태 코드 | 서버 fallback 변경이 필요 — 이번 범위 밖 |
| `og:image` / `twitter:card=summary_large_image` | 공유 이미지 자체가 없음 |
| 블로그·가이드 콘텐츠 | 꾸준히 쓸 수 있을 때 시작해야 의미가 있음 |
| 리뷰·평점 구조화 데이터 | 리뷰가 존재하지 않음 |
| 다국어(hreflang) | 서비스가 한국어 전용 |
| 페이지 속도 최적화(코드 분할 등) | 측정 후에 하는 것이 맞음. 공개 페이지는 이미 정적 HTML |
