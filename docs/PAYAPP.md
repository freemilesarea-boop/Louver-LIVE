# PayApp 정기결제 (운영 설정)

247streams의 유료 구독은 PayApp 정기결제로 청구합니다.

- 공식 문서: <https://docs.payapp.kr/dev_center01.html>
- REST endpoint: `POST https://api.payapp.kr/oapi/apiLoad.html`
  (`application/x-www-form-urlencoded`, 응답은 URL encoded `key=value`)

이 문서는 **운영자가 해야 하는 설정**과 **코드가 실제로 어떻게 동작하는지**를 적은
것입니다.

---

## 1. 가장 중요한 원칙

**정기결제 등록 성공은 결제 완료가 아닙니다.**

PayApp 공식 문서: 정기결제 등록 후 구매자가 `payurl`에서 최초 1회 결제 승인을
성공해야 이후 정기 결제가 발생합니다.

그래서 이 코드에서 유료 권한을 부여하는 지점은 **단 하나**입니다:

```
POST /api/billing/payapp/feedback   →  pay_state=4 + 전체 검증 통과
                                    →  activate_subscription(user_id, 저장된 plan_id)
```

- `rebillRegist` 성공만으로는 활성화하지 않습니다
- `returnurl` 도착만으로는 활성화하지 않습니다
- 브라우저가 호출할 수 있는 어떤 route도 `activate_subscription`을 부르지 않습니다

## 2. 환경변수

`.env`에 넣습니다. `.env.example`에는 **이름만** 있습니다.

```dotenv
PAYAPP_USERID=
PAYAPP_LINKKEY=
PAYAPP_LINKVAL=
LOUVER_PUBLIC_URL=https://247streams.kr
```

| 변수 | 용도 |
|---|---|
| `PAYAPP_USERID` | PayApp 판매자 ID. 비밀이 아니며 요청과 로그에 그대로 나옵니다 |
| `PAYAPP_LINKKEY` | 요청 인증 + feedback 검증. **비밀** |
| `PAYAPP_LINKVAL` | feedback 검증. **비밀** |
| `LOUVER_PUBLIC_URL` | feedbackurl / returnurl / failurl을 만드는 기준 주소 |
| `PAYAPP_API_URL` | (선택) 기본값은 위 공식 endpoint |

`docker-compose.yml`의 `louver` 서비스는 `.env`를 `env_file`로 읽으므로 그대로
전달됩니다. `environment:` 블록에 다시 적지 **않는** 이유는 `docker compose config`가
그 값을 화면에 출력하기 때문입니다.

**세 개 중 하나라도 없으면** 서버는 정상 부팅하고, checkout만
`"결제 시스템이 아직 설정되지 않았습니다."`를 돌려줍니다. 방송·업로드·YouTube 연결은
영향을 받지 않습니다.

`LOUVER_PUBLIC_URL`이 없으면 결제 기능 전체가 비활성화됩니다 — 이것이 없으면
PayApp에 알려줄 알림 주소가 없고, 그러면 모든 결제가 확인되지 않은 채로 남습니다.

## 3. PayApp 판매자 관리페이지에서 설정할 것

1. **정기결제 사용 신청** — 정기결제는 별도 계약/승인이 필요합니다. 승인 전에는
   `rebillRegist`가 거절됩니다.
2. **LINKKEY / LINKVAL 발급** — 관리페이지의 연동 정보에서 확인해 `.env`에 넣습니다.
3. **결제 설정 → 신용카드 활성화** — 247streams는 `openpaytype=card`로 요청합니다.
   PayApp 공식 지원 값은 `card`(신용카드)와 `phone`(휴대전화)이며, **판매자 계정의
   결제 설정이 요청값보다 우선할 수 있습니다.** 카드가 꺼져 있으면 결제창이
   기대와 다르게 열립니다.
4. **연동 URL 등록** — 관리페이지에서 아래를 등록/확인합니다. 요청마다 함께 보내지만,
   계정 설정과 어긋나면 알림이 오지 않습니다.

```
feedbackurl : https://247streams.kr/api/billing/payapp/feedback
returnurl   : https://247streams.kr/billing/complete
failurl     : https://247streams.kr/api/billing/payapp/failure
```

> `failurl`은 공식 문서에 *"결제실패 Noti URL (1회차 승인 실패는 Noti되지 않습니다)"*
> 로 적혀 있습니다. **최초 결제 실패는 알림이 오지 않으므로** 첫 결제 성공은 오직
> `pay_state=4` 알림으로만 알 수 있습니다.

## 4. 요청 필드

### `cmd=rebillRegist`

| 필드 | 값 |
|---|---|
| `userid` | `PAYAPP_USERID` |
| `goodname` | `247streams Basic` / `Pro` / `Business` |
| `goodprice` | `plans` 테이블의 `monthly_price_krw` (19900 / 39900 / 59900) |
| `recvphone` | 사용자가 checkout에서 입력. 숫자만, `01`로 시작하는 10–11자리 |
| `rebillCycleType` | `Month` |
| `rebillCycleMonth` | **가입일의 일자.** 29·30·31일이면 `90`(말일) |
| `rebillExpire` | 오늘부터 **10년 뒤**, `yyyy-mm-dd` |
| `recvemail` | 로그인 사용자의 이메일 |
| `openpaytype` | `card` |
| `feedbackurl` / `returnurl` / `failurl` | 위 세 주소 |
| `var1` | **247streams의 opaque 주문 id** |
| `var2` | plan id (참고용. 신뢰하지 않습니다) |

`rebillRegist`에는 `linkkey`/`linkval`을 보내지 않습니다.

**결제일 정책 (우리가 정한 것):** PayApp 문서는 최초 승인일과 `rebillCycleMonth`의
관계를 명시하지 않습니다. 그래서 추측하지 않고 이렇게 정했습니다 — **가입한 날의
일자에 매월 청구**하고, 매월 존재하지 않는 29·30·31일은 **말일(`90`)** 로 보냅니다.
1월 31일 가입자에게 `31`을 보내면 2월이 정의되지 않기 때문입니다.
`billing::cycle_month_for`에 있고 테스트로 고정되어 있습니다.

**만료 정책 (우리가 정한 것):** `rebillExpire`는 필수이고 PayApp에 "무기한"이 없습니다.
**10년**(`billing::REBILL_YEARS`)으로 둡니다. 실제 구독이 도달하지 않을 만큼 길고,
사람이 읽을 수 있는 날짜입니다. 구독 종료는 `rebillCancel`이 하는 일이므로, 이 날짜에
도달했다면 무언가 잘못된 것이고 운영자가 봐야 합니다.

### `cmd=rebillCancel`

공식 필수 필드 **정확히 네 개**만 보냅니다.

```
cmd=rebillCancel
userid
rebill_no
linkkey
```

`linkval`은 보내지 않습니다 (공식 문서상 필수가 아님). 테스트가 네 개뿐임을
검사합니다.

## 5. feedback 검증

`POST /api/billing/payapp/feedback` 는 **세션을 요구하지 않습니다** — PayApp의
server-to-server POST이므로 우리 쿠키가 없습니다. 대신 본문의 링크키로 인증합니다.

순서대로 전부 통과해야 합니다:

| 검사 | 비교 대상 |
|---|---|
| `userid` | `PAYAPP_USERID` |
| `linkkey` | `PAYAPP_LINKKEY` (constant-time 비교) |
| `linkval` | `PAYAPP_LINKVAL` (constant-time 비교) |
| `var1` | `billing_subscriptions.id` — 우리가 만든 주문 |
| `price` | 그 행의 `amount_krw` |
| `rebill_no` | 그 행의 `provider_subscription_id` |
| `pay_state` | `4` 일 때만 활성화 |

**등록 요청의 금액 필드는 `goodprice`이지만 feedback의 금액 필드는 `price`입니다.**

**plan은 항상 DB 행에서 가져옵니다.** callback이 `var2=business`라고 주장해도 Basic을
산 주문이면 Basic이 됩니다 (테스트로 고정).

알 수 없는 필드가 추가되어도 parser는 깨지지 않습니다.

### 응답

| 결과 | HTTP | body |
|---|---|---|
| 검증 통과 (활성화·기록·중복 무관) | 200 | `SUCCESS` |
| 검증 실패 | 400 | `FAIL` |

정확히 `SUCCESS` 문자열이며 redirect하지 않습니다. 검증 실패에 `SUCCESS`를 주지 않는
이유: 위조된 요청은 몇 번 재시도해도 성공하지 못해야 하고, 진짜 알림이 일시적 문제로
실패했다면 재시도되는 쪽이 안전합니다.

## 6. pay_state

공식 값 그대로:

| 값 | 의미 | 247streams 처리 |
|---|---|---|
| 1 | 요청 | 기록만 |
| 4 | **결제완료** | **활성화** |
| 8, 32 | 요청취소 | 기록만 |
| 9, 64 | 승인취소 | 기록 + `payment_failed` |
| 10 | 결제대기 | 기록만 |
| 70, 71 | 부분취소 | 기록 + `payment_failed` |
| 그 외 | 알 수 없음 | 기록만 (숫자를 그대로 남깁니다) |

`4` 외에는 **어떤 값도 권한을 주지 않습니다.**

## 7. 중복 호출 (idempotency)

PayApp 공식 문서는 `feedbackurl`이 여러 번 호출될 수 있다고 명시합니다.

`billing_events(provider, provider_event_key)` 의 UNIQUE 인덱스가 전부입니다.
`provider_event_key`는 PayApp의 `mul_no`(그 결제의 고유 번호)이고, 비어 있으면
`주문id:pay_state:pay_date` 의 결정적 조합을 씁니다.

같은 알림이 열 번 와도: 행 하나, 활성화 한 번, 열 번 모두 `SUCCESS`.
ledger 삽입과 상태 변경은 **한 트랜잭션**이라, 그 사이에 서버가 죽어도 결제가
기록되었는데 적용되지 않은 상태는 생기지 않습니다.

다음 달 갱신은 `mul_no`가 다르므로 새 이벤트로 기록됩니다.

## 8. 해지의 의미

`POST /api/billing/cancel` →  `cmd=rebillCancel`

**정책: 사용자가 해지를 확정하면 유료 권한을 즉시 회수합니다.** 이미 결제한
기간의 남은 일수는 유지하지 않습니다. 해지된 구독은 `cancelled`이 되고 계정은
`none`(동시 송출 0)으로 내려갑니다.

**순서가 정책보다 중요합니다.** `Payapp::cancel`은:

1. 호출자 본인의 billing record를 찾습니다 (billing id는 API 표면에 없습니다)
2. `cmd=rebillCancel` — `cmd` / `userid` / `rebill_no` / `linkkey` 네 개뿐
3. `state=1`을 확인합니다
4. 그 다음에야 한 트랜잭션으로 `status=cancelled` + `cancelled_at` + entitlement 회수

PayApp이 거부하면 **로컬은 아무것도 바뀌지 않습니다.** 오류를 반환하고, 계정은
그대로 구독 중이며 재시도할 수 있습니다. 반대 순서였다면 *PayApp은 매달 계속
청구하는데 247streams 권한만 없어진* 계정이 생길 수 있습니다 — 이 순서는 그 상태를
불가능하게 만들기 위해 존재합니다.

**운영자가 직접 부여한 요금제는 건드리지 않습니다.** 회수는
`UPDATE users SET plan_id='none' WHERE id=? AND plan_id=?`로, *그 billing record가
결제한 플랜*에 한해서만 이뤄집니다. CLI로 준 Business, 다른 구독으로 결제한 플랜,
결제된 적 없는 `pending` 등록은 그대로 남습니다.

`current_period_end`는 컬럼과 값 모두 그대로 둡니다(스키마 호환, 결제 이력).
**다만 어떤 entitlement 판단도 이 값을 읽지 않습니다.** 기간 만료 스케줄러도
필요 없습니다.

### 이전 정책이 남긴 행 정리

해지했는데 권한이 남아 있는 계정(`billing.status`는 cancelled인데 `users.plan_id`는
유료)은 이전 정책에서 만들어진 것입니다. 부팅 migration이 production DB의 요금제를
자동으로 바꾸지는 않습니다. 운영자가 직접:

```bash
docker compose exec app louver-server --audit-billing          # 읽기만 함
docker compose exec app louver-server --audit-billing --fix --yes
```

쿼리는 billing record와 해당 사용자의 entitlement를 join해서 *해지됐는데 아직
그 플랜을 들고 있는* 행만 찾습니다. "Basic 사용자 전부"를 대상으로 하는 경로는
없습니다.

## 9. 요금제 변경

**한 계정에 정기결제 등록은 하나입니다.** PayApp이 아직 붙들고 있는 등록
(`pending` / `active` / `payment_failed`)이 있으면 다른 플랜 checkout을 거부합니다:

> 현재 구독을 해지한 후 요금제를 변경할 수 있습니다.

기존 `rebill`을 남겨두고 새 `rebill`을 하나 더 만들면 매월 두 번 청구되고, PayApp은
첫 번째가 있다는 것을 모릅니다.

## 10. 비밀 취급

| 값 | 어디에 | 로그에 |
|---|---|---|
| `PAYAPP_USERID` | 환경변수 | 그대로 (비밀 아님) |
| `PAYAPP_LINKKEY` | 환경변수만. **DB에 저장하지 않습니다** | `[REDACTED]` |
| `PAYAPP_LINKVAL` | 환경변수만. **DB에 저장하지 않습니다** | `[REDACTED]` |
| 카드 정보 | **받지 않습니다.** PayApp 결제창에서 직접 입력 | — |

- `billing_events`는 raw callback 본문을 저장하지 않습니다. PayApp이 우리 링크키를
  본문에 담아 보내므로, 그것을 저장하는 테이블은 그것을 유출하는 테이블입니다.
- `Config`의 `Debug` 구현이 두 키를 `[REDACTED]`로 바꿉니다.
- `var1`/`var2`에는 이메일·사용자 id·비밀이 들어가지 않습니다 (테스트로 고정).
- API 응답에 링크키가 담길 필드가 없습니다.

## 11. 문제 해결

### checkout이 "결제 시스템이 아직 설정되지 않았습니다"
세 환경변수 중 하나 또는 `LOUVER_PUBLIC_URL`이 비어 있습니다. 서버 부팅 로그에
`[louver] 결제: 설정되지 않았습니다 (...)` 가 남습니다.

### checkout이 "결제 요청이 거절되었습니다"
PayApp이 `state=0`을 돌려줬습니다. 서버 로그의
`[louver] payapp: 요청 실패 state=.. errno=.. message=..` 에 PayApp의 원문이
있습니다 (사용자에게는 보여주지 않습니다). 가장 흔한 원인은 정기결제 미승인,
LINKKEY 오류, 금액 정책 위반입니다.

### 결제했는데 요금제가 활성화되지 않음
`feedbackurl`이 도달하지 못한 것입니다. 확인 순서:
1. `LOUVER_PUBLIC_URL`이 실제 도메인인지 (IP나 localhost면 PayApp이 도달 못 함)
2. `https://247streams.kr/api/billing/payapp/feedback` 가 외부에서 열리는지
3. 서버 로그에 `[louver] payapp: feedback 거부 — ...` 가 있는지 — 있으면 어떤 검증이
   실패했는지 알려줍니다
4. `/api/billing/status`의 `subscription.status`가 `pending`이면 알림이 안 온 것이고,
   `active`면 온 것입니다

### 로그에 `feedback 거부 — 인증 실패`
`.env`의 LINKKEY/LINKVAL이 PayApp 관리페이지의 값과 다릅니다. 값은 로그에 남기지
않으므로 관리페이지와 직접 비교하세요.

### 로그에 `feedback 거부 — 금액 불일치`
저장된 주문 금액과 PayApp이 보낸 `price`가 다릅니다. 결제 도중 `plans`의 가격을
바꾸면 이렇게 됩니다. 기존 주문은 등록 당시 금액으로 청구되므로, 가격 변경은 새
주문에만 적용됩니다.

## 12. 실제 검증 상태

이 저장소의 테스트는 **가짜 PayApp**을 상대로 돕니다
(`crates/louver-cloud/tests/payapp.rs`, `apps/server/tests/http_api.rs`). CI에서
`api.payapp.kr`으로 나가는 요청은 하나도 없습니다.

운영자가 실제 PayApp 판매자 계정으로 확인했다고 알려준 항목:

- checkout → PayApp 결제창
- 1회차 실제 카드 결제
- 정기결제 등록
- `rebillCancel` (해지 요청)

**위 확인은 즉시 회수 정책 이전 코드에서 이뤄졌습니다.** 이번 변경(해지 확정 시
entitlement 즉시 회수) 이후의 production 해지 동작은 **아직 검증되지 않았습니다.**
배포 후 실제 계정으로 한 번 해지해서 다음을 확인해야 합니다:

1. PayApp 관리페이지에서 정기결제가 해지되었는지
2. `/api/billing/status`가 `cancelled` + `plan.active=false`인지
3. 새 방송 시작이 402로 거부되는지

`--audit-billing`은 배포 직후 한 번 읽기 모드로 실행해서, 이전 정책이 남긴 계정이
있는지 먼저 눈으로 확인하세요.
