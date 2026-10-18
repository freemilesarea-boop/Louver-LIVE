# 웹 대시보드만 배포하기

방송을 끊지 않고 화면만 바꾸는 방법. 여기 있는 명령은 전부 서버에 SSH 로 들어가
`~/louver-live` 에서 실행하는 것을 기준으로 합니다.

배포 전체는 [CLOUD_TESTING.md](CLOUD_TESTING.md), 장애 대응은
[OPERATIONS.md](OPERATIONS.md), 공개 페이지와 검색 노출은 [SEO.md](SEO.md) 를
보세요.

---

## 1. 왜 따로 배포해야 하는가

지금 구조에서는 웹 번들이 **서버 이미지 안에** 들어 있습니다.

```
Dockerfile:  COPY --from=web /app/apps/web/dist /srv/louver/web
             ENV LOUVER_WEB_DIR=/srv/louver/web
```

그리고 그 파일을 꺼내 주는 것은 Rust 서버 자신입니다
(`apps/server/src/lib.rs` 의 `with_web_ui`). 즉 **버튼 글자 하나를 고치려면
이미지를 다시 빌드하고 컨테이너를 다시 만들어야 합니다.**

그런데 FFmpeg 는 그 서버 프로세스가 직접 띄운 자식 프로세스입니다
(`crates/louver-core/src/streaming/ffmpeg.rs`). 컨테이너를 다시 만들면:

- 송출 중인 모든 FFmpeg 가 같이 죽습니다. 고객 방송이 끊깁니다.
- `docker compose up -d --build` 가 보내는 SIGTERM 은 처리되지 않습니다
  (`apps/server/src/main.rs` 는 `ctrl_c()` 만 기다립니다). 정리 없이 죽습니다.
- 다시 올라오면 `recover_all()` → `renew()` 가 돌면서 **YouTube 방송이 새로
  만들어질 수 있습니다** (시청 URL 이 바뀝니다). 경우에 따라 스트림 키가 다시
  쓰이거나, YouTube 가 `complete` 를 돌려주면 `give_up()` 으로 영구 포기합니다.

**그래서 화면 수정은 서버를 건드리지 않는 경로가 따로 있어야 합니다.** 그게 이
문서입니다.

## 2. 바뀌는 구조

| | 지금 | 이 문서의 구조 |
| --- | --- | --- |
| 정적 파일 | 서버 이미지 안 (`/srv/louver/web`) | 호스트 디스크 (`/srv/louver/web/releases/<버전>`) |
| 정적 파일을 주는 쪽 | Rust 서버 (`ServeDir`) | Caddy (`file_server`) |
| 화면 배포 | 이미지 재빌드 + 컨테이너 재생성 | **심볼릭 링크 교체 한 번** |
| 방송 영향 | 전부 끊김 | 없음 (프로세스에 신호조차 가지 않음) |

`/api/*` 와 `/health` 는 그대로 `louver:8080` 으로 넘어갑니다. SSE
(`/api/events`) 도 `flush_interval -1` 로 버퍼 없이 그대로 흐릅니다. 인증
쿠키도 그대로 통과합니다 — 전부 `scripts/web-split-check.sh` 가 매번 확인합니다.

디렉터리 모양:

```
/srv/louver/web/
  releases/20261012T062500Z-22dddc6/   apps/web/dist 를 그대로 풀어둔 것
  releases/20261011T131800Z-e692dd8/   하나 전. 롤백용으로 남겨둠
  current  -> releases/20261012T062500Z-22dddc6     (상대 경로 심볼릭 링크)
  previous -> releases/20261011T131800Z-e692dd8     rollback 이 돌아갈 곳
  meta/                                 버전별 배포 시각
```

`current` 가 **상대 경로**인 것이 중요합니다. 호스트에서는
`/srv/louver/web/current`, 컨테이너 안에서는 `/srv/web/current` 로 같은 링크를
읽기 때문입니다.

## 3. 파일을 서버로 가져오기

운영 디렉터리는 git 저장소가 아닙니다 — `scripts/deploy-vps.sh:65` 가 tar 에서
`./.git` 을 제외합니다. 그래서 `git pull` 로 가져올 수 없고, 디렉터리 전체를
덮어쓰는 것도 안 됩니다. 아래가 **신규 파일 6개만** 놓는 경로입니다.

### 무엇을 보내는가

| 모드 | 대상 경로 |
| --- | --- |
| 644 | `deploy/web/Caddyfile` |
| 644 | `docker-compose.web.yml` |
| 644 | `docs/WEB_DEPLOY.md` |
| 755 | `scripts/deploy-web.sh` |
| 755 | `scripts/web-split-preflight.sh` |
| 755 | `scripts/web-split-verify.sh` |

**SHA-256 은 여기 적지 않습니다.** 이 문서 자신이 그 목록에 들어 있어서, 해시를
여기 쓰면 문서를 고칠 때마다 자기 자신에 대한 값이 틀어집니다. 권위 있는 목록은
`scripts/install-web-split.sh` 의 `SHAS` 블록 하나이고, 그것이 설치할 내용의
정의입니다. 확인은:

```bash
sed -n '/^SHAS=/,/"$/p' scripts/install-web-split.sh
```

`scripts/deploy-web.test.mjs` 가 그 블록이 실제 파일과 일치하는지 CI 에서
확인하므로, 파일을 고치고 매니페스트를 잊으면 빌드가 깨집니다.

**제외한 것**: `.env.example` 과 `docs/OPERATIONS.md` 는 기존 파일 수정이고
런타임에 필요하지 않으므로 보내지 않습니다 — 덕분에 "기존 파일 변경 0건" 이
절대적입니다. `web-split-check.sh`, `web-split-stub-api.mjs`,
`deploy-web.test.mjs` 는 로컬 테스트 하네스이고 서버에서 실행할 것이 아닙니다.
`install-web-split.sh` 자체도 설치되지 않습니다 — `/tmp` 에서 돌리는 도구입니다.

### 작업용 PC에서

```bash
cd <Louver-LIVE 저장소>
git fetch origin claude/frontend-independent-deploy

# 커밋에서 바로 만든다. 두 번 만들면 바이트까지 같은 tar 가 나오므로,
# 같은 아카이브인지 누구나 다시 만들어 대조할 수 있다.
git archive --format=tar -o /tmp/louver-web-split.tar <커밋> -- \
  docker-compose.web.yml deploy/web/Caddyfile scripts/deploy-web.sh \
  scripts/web-split-preflight.sh scripts/web-split-verify.sh docs/WEB_DEPLOY.md

sha256sum /tmp/louver-web-split.tar          # 서버에서 대조할 값
tar -tvf  /tmp/louver-web-split.tar          # 6개 파일 + 디렉터리 3개. 눈으로 확인
sha256sum scripts/install-web-split.sh       # 설치 도구 자체의 값

scp /tmp/louver-web-split.tar scripts/install-web-split.sh <user>@<server>:/tmp/
```

### 서버에서

```bash
cd ~/louver-live && ls docker-compose.yml        # 여기가 맞는지 먼저
sha256sum /tmp/louver-web-split.tar              # 위에서 받은 값과 대조
sha256sum /tmp/install-web-split.sh              # 설치 도구도 대조

STAGE=$(mktemp -d) && mkdir -p "$STAGE/tree"
tar -xpf /tmp/louver-web-split.tar -C "$STAGE/tree"   # 스테이징에만 전개

bash /tmp/install-web-split.sh preflight "$STAGE/tree" ~/louver-live   # ← 통과해야 다음
bash /tmp/install-web-split.sh install   "$STAGE/tree" ~/louver-live
bash /tmp/install-web-split.sh verify                  ~/louver-live
```

전개는 스테이징에만 하므로, 전송이 잘려도 운영 디렉터리에는 닿지 않습니다.
`preflight` 가 거부하는 것: 아카이브 내용이 매니페스트와 다름, 대상에 다른 내용의
파일이 있음, 대상 경로가 심볼릭 링크이거나 디렉터리 밖으로 나감, 쓸 수 없는
디렉터리, 디스크 부족, 읽을 수 없는 저널.

### 실패했을 때

```bash
bash /tmp/install-web-split.sh status  ~/louver-live    # 저널과 디스크가 맞는지
bash /tmp/install-web-split.sh recover ~/louver-live    # 이 설치가 만든 것만 되돌림
```

**파일 6개를 한 번에 원자적으로 설치할 수는 없습니다.** 개별 파일은 원자적입니다
— 같은 디렉터리에 임시 이름으로 복사한 뒤 rename 하므로 반쯤 쓰인 파일을 누구도
보지 못합니다. 하지만 집합은 아니어서, 실패하면 0~5개가 설치된 상태로 남습니다.
저널이 그 범위를 사실로 기록하고 `recover` 가 정확히 그만큼만 되돌립니다.

저널은 **먼저 쓰고 나중에 확인하는** 방식입니다. 운영 디렉터리 밖
(`~/louver-web-split-install.journal`)에 두 줄씩 남습니다:

```
PLAN <경로> <받아야 할 해시>     복사 전
FILE <경로> <받은 해시>          rename 후
```

두 줄인 이유는 강제 종료가 어디든 떨어질 수 있고 **양쪽 모두 복구 가능해야**
하기 때문입니다:

| 죽은 지점 | 디스크 상태 | recover |
| --- | --- | --- |
| PLAN 쓴 뒤, rename 전 | 그 파일 없음 | 되돌릴 것 없음 |
| rename 뒤, FILE 쓰기 전 | 파일 있음, 저널엔 PLAN 만 | PLAN 의 해시와 같으면 **우리 것이 증명되므로** 제거 |
| 위와 같으나 내용이 다름 | 제3자가 쓴 것 | **남기고 보고**, 0 이 아닌 코드로 종료 |

각 줄은 다음 단계 전에 `sync` 로 플러시되므로 저널이 디스크보다 뒤처질 수
없습니다.

**recover 가 하지 않는 것**: 설치 당시 바이트와 다른 파일은 지우지 않고 이름을
들어 보고하고 0 이 아닌 코드로 끝냅니다. 디렉터리는 `rmdir` 로만 지우므로 남은
파일이 있으면 그 부모도 남습니다. 자기가 만들지 않은 파일은 건드리지 않습니다.
저널을 읽을 수 없으면 **추측하지 않고 멈춥니다** — 그때는 위 표의 해시로 6개
경로를 사람이 대조해야 합니다. 저널이 유실되면 역시 거부하고, `verify` 로
매니페스트 일치만 확인할 수 있습니다.

설치와 복구는 `flock` 으로 한 번에 하나만 돕니다. 동시에 두 번 실행하면 두 번째가
거부됩니다. 강제 종료는 lock 을 남기지 않습니다(파일 서술자가 닫히며 해제).

### 되돌리기

전달한 6개는 전부 신규 파일이므로, 되돌리기는 `recover` 하나입니다. 기존 파일을
건드린 적이 없으므로 백업 복원이 필요 없습니다.

## 4. 한 번만 하는 준비 (승인 필요)

컨테이너에 마운트를 추가하는 것이므로 **`caddy` 컨테이너는 이때 한 번
재생성됩니다.** `louver` 는 건드리지 않습니다 — `--no-deps` 가 그것을 보장합니다.

재생성 중 HTTPS 프런트 도어가 내려가는 시간은 **최대 10초쯤**입니다. Caddy 는
SIGTERM 에서 graceful 하게 멈추며 **열려 있는 응답을 끊지 않습니다**(실측: SIGTERM
이후에도 SSE 가 계속 흘렀습니다). 그래서 Docker 는 stop grace period 를 다
기다린 뒤 SIGKILL 하고, 그 다음에 새 컨테이너를 띄웁니다. `docker-compose.yml`
에 `stop_grace_period` 가 없으므로 기본 10초입니다.

**송출은 영향받지 않습니다.** RTMP 는 louver 안의 FFmpeg 가 YouTube 로 직접
내보내는 outbound 이고, Caddy 는 그 경로에 없습니다. louver 의 `:8080` 포트
매핑도 그대로이므로 관리 경로는 유지됩니다. 열려 있던 대시보드의 SSE 는 SIGKILL
시점에 끊기고, 브라우저의 `EventSource` 가 스스로 다시 연결합니다(그리고
`transport.ts` 에 3초 폴링 폴백이 있습니다).

```bash
# 1) 호스트에 공간을 만든다
sudo mkdir -p /srv/louver/web/releases

# 2) 지금 서비스되고 있는 번들을 그대로 첫 릴리스로 꺼낸다.
#    돌고 있는 컨테이너를 아예 건드리지 않기 위해, 같은 이미지로 멈춘
#    컨테이너를 하나 새로 만들어 거기서 복사하고 지운다.
IMG=$(docker compose images -q louver)
docker create --name louver-web-copy "$IMG" > /dev/null
docker cp louver-web-copy:/srv/louver/web /tmp/web-current
docker rm louver-web-copy > /dev/null
scripts/deploy-web.sh release /tmp/web-current --root /srv/louver/web

# 3) .env 에 한 줄 추가한다. 이것이 없으면 다음 scripts/deploy-vps.sh 가
#    분리 구조를 조용히 되돌린다 (아래 6번 F1 참고).
echo 'COMPOSE_FILE=docker-compose.yml:docker-compose.web.yml' >> .env

# 4) 전환 전 점검. 아무것도 바꾸지 않는 읽기 전용 검사이며,
#    실패하면 전환하지 않는다.
scripts/web-split-preflight.sh

# 5) Caddy 만 새 설정으로 올린다. --no-deps 를 빼면 안 된다.
#    (3번을 했다면 -f 두 개는 생략해도 된다 — COMPOSE_FILE 이 대신한다.)
docker compose -f docker-compose.yml -f docker-compose.web.yml \
    --profile https up -d --no-deps caddy

# 6) 합격 판정
docker compose -f docker-compose.yml ps          # louver 의 CREATED 가 그대로여야 한다
scripts/web-split-verify.sh https://247streams.kr # ARMED 가 나와야 한다
```

6번의 두 줄이 이 작업의 합격 기준입니다. `louver` 컨테이너의 `CREATED` 가
바뀌지 않았다면 방송은 한 번도 끊기지 않았습니다.

### 전환 뒤에는 화면이 두 곳에서 나옵니다

`:8080` 포트 매핑과 서버 이미지 안의 번들은 그대로 남습니다. 그래서:

| 어디로 보는가 | 무엇이 나오는가 |
| --- | --- |
| `https://<도메인>` (Caddy) | **호스트 디스크의 최신 릴리스** |
| `http://<IP>:8080` (서버 직접) | **이미지에 박힌 옛 번들** — 화면 배포를 해도 안 바뀝니다 |

의도된 성질입니다. `web-split-verify.sh` 가 바로 이 차이로 ARMED 를 판정하고,
되돌리기가 간단한 이유도 이것입니다 — 오버레이를 빼면 이미지의 번들이 그대로
화면을 냅니다. 다만 **점검할 때 IP:8080 으로 보면 "배포가 안 됐다" 고 오해합니다.**
화면을 확인할 때는 도메인으로 보세요. 그 두 번들이 하나가 되는 것은 다음 서버
배포 때이고, 그것은 점검 창에서 할 일입니다(§8).

`web-split-preflight.sh` 가 보는 것 (전부 읽기 전용 — `config`, `ps`,
`up --dry-run` 뿐이고 파일에 플래그 없는 `up` 은 존재하지 않습니다):

1. `deploy-web.sh check` — 릴리스가 준비되어 있는지. **실패하면 전환 금지**
2. 오버레이가 `louver` 서비스를 바꾸지 않는지 — resolve 된 정의를 바이트 비교
3. 이번 적용의 `up --dry-run` 이 `louver` 컨테이너를 건드리지 않는지
4. 지금 `louver` 컨테이너의 생성 시각 — 적용 후 비교용
5. `.env` 의 `COMPOSE_FILE` 유무 — 없으면 F1 경고
6. **그 핀을 넣은 뒤 맨몸 `up -d` 가 louver 를 재생성하는지** — 아래 F4. 전환을
   막지 않는 **경고**입니다

3번과 6번은 서로 다른 질문입니다. 3번은 *이번* 적용(`--no-deps caddy`)이 무엇을
하는지 묻고, 6번은 **그 다음에 누군가 치는 명령**이 무엇을 하는지 묻습니다. 3번의
dry-run 에는 `--no-deps` 가 들어 있어서 louver 가 나올 수가 없으므로, 6번 없이는
아무도 그 질문을 하지 않습니다.

두 검사 모두 **서비스 이름이 아니라 컨테이너 이름으로** 판정합니다. 프로젝트가
`louver-live` 라서 caddy 컨테이너가 `louver-live-caddy-1` 이고, `\blouver\b` 는
그 안에서도 매치됩니다 — dry-run 이 `Container louver-live-caddy-1  Recreate` 라고
출력하므로, 그 패턴은 **옳은 적용을 거부**했습니다.

### 왜 `check` 를 통과해야 하는가

릴리스 없이 Caddy 를 이 디렉터리로 돌리면 **아무것도 시끄럽게 실패하지
않습니다.** 실측한 결과:

```
/ 404   /dashboard 404   /pricing/ 404   /assets/...js 404   /robots.txt 404
/api/health 200   /health 200
```

Caddy 는 정상 기동하고 API 는 계속 답합니다. 즉 **방송과 API 는 무사한 채 대시보드만
백지**가 되고, 겉보기에는 Caddy 설정 오류처럼 보입니다. `deploy-web.sh check` 는
그 상태를 만들 수 있는 모든 조건을 미리 거부합니다 — 릴리스 없음, 스테이징만 됨,
끊어진 링크, 절대 경로 링크, `index.html` 없음, 빈 `assets/`.

### 되돌리기

```bash
sed -i '/^COMPOSE_FILE=/d' .env                        # 먼저 핀을 뺀다
docker compose --profile https up -d --no-deps caddy    # 원래 Caddyfile 로
```

오버레이를 빼면 Caddy 는 다시 전부 `louver:8080` 으로 넘깁니다. 서버 이미지 안의
번들이 그대로 남아 있으므로 화면도 그대로 나옵니다. 되돌리는 쪽도 `louver` 를
건드리지 않습니다. `.env` 의 핀을 먼저 지우지 않으면 이 명령도 오버레이를
적용합니다.

## 5. 평소의 화면 배포

```bash
npm run build:cloud                                     # apps/web/dist
scripts/deploy-web.sh release apps/web/dist --root /srv/louver/web
```

끝입니다. Caddy 는 요청마다 링크를 따라가므로 **reload 도 필요 없습니다.** 로컬
검증에서 열려 있던 SSE 스트림은 이 교체를 두 번 거치고도 39개 이벤트를 한 번도
끊기지 않고 받았습니다.

```bash
scripts/deploy-web.sh list --root /srv/louver/web       # 무엇이 돌고 있는지
scripts/deploy-web.sh rollback --root /srv/louver/web   # 직전 버전으로
scripts/deploy-web.sh prune --keep 5 --root /srv/louver/web
```

`prune` 은 `--keep 1` 을 거부하고, 지금 쓰이는 버전과 롤백 대상은 순서와
무관하게 남깁니다.

#### 활성화가 중간에 끊겼다면

심볼릭 링크 **두 개**를 한 번에 바꿀 수는 없습니다. `activate` 는 `previous` 를
먼저, `current` 를 나중에 씁니다 — 그 사이에서 죽으면 둘이 같은 릴리스를 가리킵니다.
이 순서가 안전한 쪽입니다: 반대로 하면 `previous` 가 한 세대 더 뒤를 가리켜
**롤백이 조용히 엉뚱한 버전으로** 갑니다.

다만 그 상태를 작동하는 롤백으로 착각해서는 안 됩니다. `activate` 는 이미 current
인 릴리스를 요청받으면 그냥 돌아가므로, 예전에는 `rollback` 이 "already current" 를
찍고 **0 으로 끝났습니다** — 성공했다고 보고하면서 아무것도 옮기지 않은 것입니다.
지금은 거부하고 빠져나갈 길을 알려줍니다:

```bash
scripts/deploy-web.sh list --root /srv/louver/web
scripts/deploy-web.sh activate <버전> --root /srv/louver/web
```

`deploy-web.sh check` 도 이 상태를 **note 로** 알립니다 — 전환을 막지는 않습니다.
서비스되고 있는 것은 온전한 릴리스 하나이고, 비상용으로 남겨둔 명령 하나가 장전되지
않았다는 뜻이기 때문입니다. 필요해지기 전에 알아야 하는 종류의 사실입니다.

### 캐시 정책

| 경로 | 헤더 | 이유 |
| --- | --- | --- |
| `/assets/*` | `public, max-age=31536000, immutable` | Vite 가 파일 이름에 내용 해시를 넣으므로 이름이 같으면 내용도 같다 |
| 그 밖의 전부 | `no-cache` | "캐시 금지"가 아니라 "쓰기 전에 확인". 304 로 끝나고, 새 `assets/index-<해시>.js` 를 바로 보게 된다 |

그래서 롤백도 즉시 보입니다. 되돌린 `index.html` 은 다음 요청에서 재검증되고,
그것이 가리키는 이전 해시 파일은 여전히 디스크에 있습니다.

### 이전 빌드의 파일은 계속 제공된다

교체 직전에, **나가는 릴리스의 `assets/` 를 들어오는 릴리스로 하드링크로
넘깁니다.** 오래 열어둔 탭이 이전 빌드의 `index.html` 에 적힌 해시 파일을
요청할 때 404 가 나지 않도록 하기 위한 것입니다. 404 가 나면 브라우저는 모듈
자리에서 HTML 을 받아 "HTML 에 문법 오류" 를 보고하고, 화면은 백지가 됩니다.

- 해시 파일명은 내용이 같으면 이름도 같으므로 **합집합이 항상 잘 정의됩니다.**
- `-n` 이므로 들어오는 릴리스가 자기 이름으로 제공하는 것은 **절대 덮어쓰지
  않습니다.**
- 하드링크이므로 **바이트는 디스크에 한 번만** 존재합니다. 몇 세대가 참조해도
  같습니다 (실측: 3세대 보관 시 64K).
- **롤백에도 같은 방향으로 동작합니다** — 교체되는 쪽이 자기 파일을 넘기므로,
  v2 를 보고 있던 사람도 v1 로 되돌린 뒤에 깨지지 않습니다.
- `prune` 이 원본 릴리스 디렉터리를 지워도 **바이트는 남습니다.** 최신 릴리스가
  같은 inode 를 가리키고 있기 때문입니다 (실측).

끄려면 `--no-carry` 를 줍니다. 그러면 다른 빌드의 청크는 다시 404 가 됩니다 —
`scripts/web-split-check.sh` 가 그 사실을 양방향으로 확인합니다.

지금 번들에는 코드 스플리팅이 없어서(빌드된 JS 에 `import(` 0건) 이미 로드된
탭은 추가 자산을 요청하지 않습니다. 그래도 넘겨두는 이유는, 라우트 단위 lazy
import 를 켜는 날 이 보호 장치가 **먼저** 있어야 하기 때문입니다.

## 6. 위험과 그 대비

| 위험 | 왜 생기는가 | 대비 |
| --- | --- | --- |
| 빈 디렉터리를 배포 | 빌드가 실패했는데 모르고 올림 | `deploy-web.sh` 가 `index.html` 과 `assets/` 없는 디렉터리를 거부 |
| 반쯤 복사된 상태가 노출 | 복사 중에 요청이 들어옴 | 숨은 디렉터리에 복사하고 `mv` 로 넣음. `current` 는 완성된 것만 가리킴 |
| 없는 JS 를 HTML 로 응답 | SPA fallback 이 `/assets` 까지 먹음 | `/assets/*` 와 확장자가 있는 경로는 fallback 없이 404. 로컬 검증에서 실제로 이 사고가 재현되어 고친 것 |
| 공개 페이지가 SPA 껍데기로 바뀜 | 디렉터리는 `try_files` 매치가 아님 | `{path}/index.html` 후보를 둠. `/pricing` 등 7개 페이지 제목을 실제 번들로 확인 |
| 인증서를 매번 다시 받음 | 오버레이가 `/data` 마운트를 빠뜨림 | 오버레이가 볼륨 4개를 전부 다시 적음 |
| `louver` 가 같이 재생성됨 | `--no-deps` 를 빼먹음 | 위 명령에 포함. 적용 후 `docker compose ps` 의 `CREATED` 로 확인 |
| 버전이 쌓여 디스크를 먹음 | 릴리스마다 번들 하나 | `prune --keep 5`. 번들 하나는 수 MB 수준 |
| Caddy 설정이 두 파일로 갈림 | 루트 `Caddyfile` 과 `deploy/web/Caddyfile` | 루트 쪽은 건드리지 않고 둠. 오버레이를 쓰는 동안은 `deploy/web/Caddyfile` 만 읽힌다 |
| **기존 배포가 분리 구조를 조용히 해제 (F1)** | `scripts/deploy-vps.sh` 가 맨몸 `docker compose --profile https up -d --build` 로 끝난다 → 오버레이 없이 Caddy 재생성. 화면은 이미지 번들로 계속 나오므로 **아무도 눈치채지 못한다** | `.env` 에 `COMPOSE_FILE=docker-compose.yml:docker-compose.web.yml`. `.env` 는 배포 tar 에서 제외되고 `set_env` 는 `LOUVER_*` 만 고치므로 재배포를 견딘다. 배포 직후 `web-split-verify.sh` 로 확인 |
| 롤백이 아무것도 안 하면서 성공으로 보임 | `activate` 가 링크 두 개를 차례로 쓰므로 그 사이에 죽으면 `previous` 와 `current` 가 같아진다 | `rollback` 이 거부하고 `activate <버전>` 을 안내한다. `check` 는 note 로 알린다. 테스트 2개 — 위 5절 |
| 전환 뒤 `:8080` 이 옛 화면을 준다 | 포트 매핑과 이미지 안 번들이 그대로 남는다 | 의도된 성질이고 `web-split-verify.sh` 의 판정 근거다. 확인은 도메인으로 — 위 4절 |
| **핀이 남기는 지뢰 (F4)** | F1 대비로 `.env` 에 `COMPOSE_FILE=` 을 넣는데, `.env` 는 louver 의 `env_file` 입니다 (`docker-compose.yml`). Compose 가 `env_file` 을 서비스 environment 로 풀면 louver 의 config hash 가 바뀌고, 그러면 **맨몸 `docker compose up -d` 가 louver 를 재생성** 합니다 — 전에는 아무 일도 안 했을 수 있는 명령이 모든 방송을 끊는 명령이 됩니다 | preflight 6번이 `up -d --dry-run` 으로 **묻습니다**(추론하지 않습니다 — env_file 이 hash 에 들어가는지는 Compose 버전에 달렸습니다). 고칠 것은 없습니다: 돌고 있는 컨테이너를 재생성 없이 바뀐 설정에 맞출 수 없고, 핀을 빼면 F1 이 돌아옵니다. 그래서 **경고**이고, 전환을 막지 않습니다. 이후 front door 만 손볼 때는 `docker compose up -d --no-deps caddy` |
| **릴리스 없이 전환 (F2)** | 순서를 건너뜀 | `deploy-web.sh check` / `web-split-preflight.sh` 가 거부. 통과 전에는 전환하지 않는다 |
| **이전 빌드 해시 404 (F3)** | 해시 번들 배포의 일반 성질. 지금 구조에서도 같다 | 교체 직전 하드링크 이관(위 4절). `--no-carry` 로만 끌 수 있다 |
| `/pricing` 과 `/pricing/` 이 둘 다 200 | 지금 서버는 `/pricing` 에 307 로 답한다. Caddy 는 바로 200 으로 답한다 | 프리렌더 페이지 전부가 `rel="canonical"` 로 슬래시 버전을 가리키고 `sitemap.xml` 도 슬래시만 싣는다. 리다이렉트를 원하면 Caddy 에 한 줄 추가 — 별도 승인 사항 |

## 7. 손대기 전에 확인하는 방법

```bash
CADDY_BIN=$(command -v caddy) scripts/web-split-check.sh
```

실제 Caddy 를 띄워 `deploy/web/Caddyfile` **그 파일을** 검사합니다. 파일 안의
주소가 모두 환경 변수인 이유가 이것입니다 — 배포될 설정과 검사된 설정이 같은
파일이어야 합니다. 60개 항목을 봅니다: 화면, `/api/*`, `/health`, 세션 쿠키,
열려 있는 SSE, v1→v2, v2→v1, 이전 빌드의 JS/CSS 가 계속 제공되는지, 없는 파일의
404, SPA 새로고침, 공개 페이지(슬래시 포함), 캐시 헤더, `check` 가 막아야 할
상태를 실제로 막는지, 실패한 배포가 살아 있는 릴리스를 건드리지 않는지, 그리고
돌고 있는 FFmpeg 가 같은 PID 로 살아 있는지. `docker` 와 `systemctl` 은 PATH 에서
가로채 두므로, 호출되기만 해도 실패합니다.

운영에서 쓰는 두 개:

```bash
scripts/web-split-preflight.sh                      # 전환 전, 읽기 전용
scripts/web-split-verify.sh https://247streams.kr   # 전환 후 / 배포 후, 읽기 전용
```

`web-split-verify.sh` 는 **존재할 수 없는 해시 파일** 을 하나 요청해서 판정합니다.
분리 구조가 앞에 있으면 404 이고(`handle /assets/*` 에 fallback 이 없으므로),
없으면 서버의 `ServeDir::new(dir).fallback(ServeFile::new(index))` 가 **200 과
HTML** 을 돌려줍니다. 실제 `louver-server` 를 띄워 확인한 동작입니다:

```
/assets/__web_split_probe__1.js   →  200  text/html   (오늘의 경로)
                                  →  404             (분리 구조가 앞에 있을 때)
```

릴리스 스크립트 자체의 단위 테스트는 CI 에서 돕니다:

```bash
npx vitest run --config vitest.tooling.config.ts scripts/deploy-web.test.mjs
```

## 8. 아직 하지 않은 것

이 구조는 **화면만** 서버에서 떼어냅니다. Rust 서버를 무중단으로 배포하는 것은
별개의 문제이고, FFmpeg 가 서버 프로세스의 자식인 한 해결되지 않습니다. 워커를
분리하고 `flock` 로 중복 실행을 막는 설계가 선행되어야 합니다.
