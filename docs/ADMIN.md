# 247streams 관리 콘솔

`https://247streams.kr/admin` 에 있는 운영자 화면입니다. 매출, 회원, 방송, 결제,
서버 상태를 **실제 운영 데이터 그대로** 보여주고, 두 가지 조치(계정 비활성화,
방송 강제 종료)를 할 수 있습니다.

숫자를 만들어내지 않습니다. 데이터가 없으면 0 이나 "없습니다" 를 보여줍니다.

---

## 1. 관리자 만들기

서버에 SSH 로 들어가서 한 번만 실행합니다. **배포가 자동으로 실행하지 않습니다.**

```bash
docker compose exec louver louver-server --set-admin freemilesarea@gmail.com
docker compose exec louver louver-server --list-admins
docker compose exec louver louver-server --drop-admin someone@example.com   # 되돌리기
```

권한을 주는 API 는 없습니다. 일반 사용자 API 로는 어떤 방법으로도 관리자가 될 수
없고, 관리자 자신도 웹에서 다른 사람을 관리자로 만들 수 없습니다. 셸이 있는
사람만 가능합니다.

프런트엔드는 이메일을 보고 판단하지 않습니다. `users.role` 한 곳만 봅니다.

## 2. 접근 통제

| 상황 | 결과 |
| --- | --- |
| 로그인하지 않고 `/api/admin/*` | **401** |
| 로그인했지만 일반 사용자 | **403** |
| 일반 사용자가 브라우저로 `/admin` | "관리자만 열 수 있는 페이지입니다" |
| 관리자 | 콘솔 |

브라우저에서 메뉴를 숨기는 방식이 아닙니다. 모든 `/api/admin/*` 요청은 그때마다
데이터베이스에서 `users.role` 을 다시 읽습니다. 쿠키를 직접 만들거나 URL 을
외워서 들어와도 서버가 거절합니다.

비활성화된 계정은 로그인 자체가 거절되고, 이미 가지고 있던 세션도 끊깁니다.

## 3. 화면

| 화면 | 답하는 질문 |
| --- | --- |
| 대시보드 | 오늘 얼마 벌었나 / 이번 달은 / MRR 은 / 회원은 몇 명이고 오늘 몇 명 들어왔나 / 유료는 몇 명인가 / 지금 몇 개가 송출 중인가 / 문제가 있는가 / 디스크는 괜찮은가 |
| 매출 | 기간별 매출, 신규와 갱신의 비율, 요금제별 매출, 최근 결제 내역 |
| 회원 | 검색·필터·정렬, 한 명의 상세(요금제·결제·저장공간·YouTube·방송), 계정 비활성화 |
| 방송 | 지금 돌고 있는 것, 예약, 오류, 강제 종료 |
| 결제 | 결제 내역, 해지 목록, 결제-권한 불일치 |
| 시스템 | `/health`, CPU/메모리/디스크, 저장공간 상위 계정, 최근 방송 오류 |
| 감사 로그 | 관리자가 무엇을 했는지 |

## 4. 숫자의 정의

**매출 = 실결제 매출.** `billing_events` 중 PayApp 이 성공(`pay_state = '4'`)이라고
알려준 건의 금액 합계입니다. 회원 수 × 요금제 가격이 아닙니다.

- 같은 결제에 대한 PayApp 콜백이 두 번 와도 한 번만 계산합니다
  (`UNIQUE(provider, provider_event_key)`).
- 실패·취소 통보는 매출이 아닙니다.
- **환불 데이터는 PayApp 이 보내주지 않습니다.** 그래서 "순매출" 을 계산하지
  않고 **실결제 매출** 이라고만 씁니다. 취소 통보 건수는 매출 화면 아래에
  따로 표시합니다.
- 신규 / 갱신은 그 결제가 해당 정기결제의 첫 성공 결제인지로 나눕니다.

**MRR = 지금 `active` 인 정기결제의 월 금액 합계** 입니다
(`billing_subscriptions.status = 'active'`). 대기·해지·결제실패·등록실패는
빠집니다. 가격은 이 문서나 프런트엔드에 적혀 있지 않고 결제 등록 시점의 금액과
`plans` 테이블을 씁니다.

**ARPU = 이번 달 실결제 매출 ÷ 현재 정기결제가 살아 있는 계정 수** 입니다.
유료 계정이 없으면 계산하지 않고 `—` 로 둡니다.

**전환율 같은 과거 추정치는 보여주지 않습니다.** 가입·해지 시점의 이벤트를
전부 보관하고 있지 않기 때문에, 지금 상태로 과거를 역산하지 않습니다.

**하루의 경계는 한국 시간(Asia/Seoul)** 입니다. 데이터베이스는 UTC 로 저장하고
집계할 때만 9시간을 더합니다(`date(processed_at, '+9 hours')`). 저장된 타임스탬프는
바꾸지 않습니다.

## 5. 조치

### 방송 강제 종료

사용자가 중지 버튼을 누른 것과 **완전히 같은 경로**로 끕니다. FFmpeg 프로세스를
직접 kill 하지 않습니다.

- `desired_state` 가 `stopped` 로 바뀌므로 워치독이 다시 살리지 않습니다.
- YouTube 방송은 정상적으로 `complete` 처리됩니다.
- 그 방송 하나만 멈춥니다. 같은 계정의 다른 방송이나 다른 사람의 방송은
  건드리지 않습니다.
- 감사 로그에 남습니다.

### 계정 비활성화

- 로그인이 막히고 기존 세션이 끊깁니다.
- 그 계정에서 돌고 있던 방송은 같은 정상 경로로 종료됩니다.
- **데이터는 지우지 않습니다.** 회원·결제·미디어·방송 기록이 그대로 남고,
  다시 활성화하면 원래대로 돌아옵니다. 하드 삭제 기능은 없습니다.
- 자기 자신은 비활성화할 수 없습니다.
- PayApp 정기결제는 자동으로 해지되지 않습니다. 필요하면 사용자 본인이 해지하거나
  운영자가 PayApp 에서 처리해야 합니다. 화면에도 그렇게 적혀 있습니다.

### 콘솔이 하지 않는 것

요금제를 콘솔에서 바꾸는 기능은 넣지 않았습니다. PayApp 쪽 정기결제를 함께 바꾸지
않으면 "결제는 계속되는데 권한만 다른" 상태가 되기 때문입니다. 결제와 관련된
변경은 PayApp 을 거치는 기존 경로(사용자의 결제/해지, `--audit-billing`)로만
합니다. 콘솔의 결제 화면은 불일치를 **보여주기만** 합니다.

파일 다운로드 기능도 없습니다. 운영자가 회원의 영상을 내려받을 이유가 없습니다.

## 6. 감사 로그

관리자가 무엇인가를 바꿀 때마다 한 줄이 남습니다: 언제, 누가(id·이메일), 무엇을,
어느 대상에, 바뀌기 전·후 값, 그리고 입력한 사유.

**비밀번호, PayApp LINKKEY/LINKVAL, OAuth client secret, YouTube access/refresh
token, 스트림 키는 감사 로그에 저장되지 않습니다.** 사유 입력란에 그런 문자열이
들어가면 저장 전에 지웁니다.

읽기(화면을 열어 본 것)는 남기지 않습니다. 남기면 로그가 조치 기록이 아니라
접속 기록이 되어 정작 중요한 줄을 찾을 수 없게 됩니다.

## 7. 보안

- 관리자에게도 스트림 키, access/refresh token, client secret, 비밀번호 해시는
  보여주지 않습니다. 서버가 응답에 넣지 않습니다.
- 결제 내역의 PayApp 참조번호(`mul_no`)는 뒤 네 자리만 보여줍니다. 정기결제 번호는
  사용자 본인의 결제 화면에도 나오는 값이라 회원 상세에 그대로 둡니다.
- 다른 사람의 id 를 넣어도 일반 사용자는 403 입니다.
- 최근 오류 화면은 방송 로그에서 최근 것만 읽습니다. 컨테이너의 stdout 전체를
  데이터베이스에 쌓지 않습니다.

## 8. 성능

집계는 전부 SQL 이 합니다. 행을 전부 읽어 애플리케이션에서 더하지 않습니다.
목록은 기본 50개, 최대 100개입니다.

콘솔을 위해 추가한 인덱스(전부 additive):

```
idx_users_created        users(created_at)
idx_billing_event_paid   billing_events(pay_state, processed_at)
idx_billing_status       billing_subscriptions(status)
idx_broadcast_runtime    broadcasts(runtime_state)
```

결제 대행사 API 를 주기적으로 호출하지 않습니다. 콘솔이 보는 것은 전부 우리
데이터베이스입니다.

## 9. 배포

이 변경에는 새 컬럼 두 개(`users.role`, `users.disabled_at`), 새 테이블 하나
(`admin_audit`), 인덱스 네 개가 들어 있습니다. 전부 추가만 하며 기존 데이터를
바꾸거나 지우지 않습니다.

```bash
# 로컬에서
npm run verify
node scripts/release-smoke.mjs

# 배포 (로컬에서 실행합니다)
scripts/deploy-vps.sh root@2.28.225.85 \
  --domain 247streams.kr \
  --email freemilesarea@gmail.com \
  --user freemilesarea@gmail.com

# 배포 후 한 번 (서버에서)
docker compose exec louver louver-server --set-admin freemilesarea@gmail.com
```

배포 전에 데이터베이스를 백업하세요 — [OPERATIONS.md](OPERATIONS.md) 2번.

되돌리려면 **로컬에서 되돌리고 다시 배포** 합니다. 서버의 `/root/louver-live` 는
git 저장소가 아니므로 서버에서 `git revert` 나 `git pull` 을 할 수 없습니다.
컬럼과 테이블은 남겨 두어도 됩니다 — 이전 빌드는 읽지 않습니다.

## 10. 확인

```bash
cargo test -p louver-cloud --test admin_queries   # 집계·감사 로그
cargo test -p louver-server --test admin_api      # 401/403/IDOR/비밀값
npx vitest run apps/web/src/admin                 # 화면
```
