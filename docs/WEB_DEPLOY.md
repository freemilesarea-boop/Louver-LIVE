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

## 3. 한 번만 하는 준비 (승인 필요)

컨테이너에 마운트를 추가하는 것이므로 **`caddy` 컨테이너는 이때 한 번
재생성됩니다.** `louver` 는 건드리지 않습니다 — `--no-deps` 가 그것을 보장합니다.
HTTPS 리스너가 1~2초 내려가지만 송출은 영향받지 않습니다. RTMP 는 서버가 YouTube
로 직접 내보내는 것이고, Caddy 는 그 경로에 없습니다.

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

# 3) 확인: current 가 상대 링크이고 index.html 이 있어야 한다
scripts/deploy-web.sh list --root /srv/louver/web
readlink /srv/louver/web/current

# 4) Caddy 만 새 설정으로 올린다. --no-deps 를 빼면 안 된다.
docker compose -f docker-compose.yml -f docker-compose.web.yml \
    --profile https up -d --no-deps caddy

# 5) 즉시 확인
curl -sI https://247streams.kr/ | grep -i cache-control        # no-cache
curl -s https://247streams.kr/api/health                        # 서버가 답해야 한다
curl -s -o /dev/null -w '%{http_code}\n' https://247streams.kr/assets/nope.js   # 404
docker compose ps                                               # louver 의 생성 시각이 그대로여야 한다
```

5번의 마지막 줄이 이 작업의 합격 기준입니다. `louver` 컨테이너의 `CREATED` 가
바뀌지 않았다면 방송은 한 번도 끊기지 않았습니다.

### 되돌리기

```bash
docker compose --profile https up -d --no-deps caddy    # 원래 Caddyfile 로
```

오버레이를 빼면 Caddy 는 다시 전부 `louver:8080` 으로 넘깁니다. 서버 이미지 안의
번들이 그대로 남아 있으므로 화면도 그대로 나옵니다. 되돌리는 쪽도 `louver` 를
건드리지 않습니다.

## 4. 평소의 화면 배포

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

### 캐시 정책

| 경로 | 헤더 | 이유 |
| --- | --- | --- |
| `/assets/*` | `public, max-age=31536000, immutable` | Vite 가 파일 이름에 내용 해시를 넣으므로 이름이 같으면 내용도 같다 |
| 그 밖의 전부 | `no-cache` | "캐시 금지"가 아니라 "쓰기 전에 확인". 304 로 끝나고, 새 `assets/index-<해시>.js` 를 바로 보게 된다 |

그래서 롤백도 즉시 보입니다. 되돌린 `index.html` 은 다음 요청에서 재검증되고,
그것이 가리키는 이전 해시 파일은 여전히 디스크에 있습니다.

## 5. 위험과 그 대비

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

## 6. 손대기 전에 확인하는 방법

```bash
CADDY_BIN=$(command -v caddy) scripts/web-split-check.sh
```

실제 Caddy 를 띄워 `deploy/web/Caddyfile` **그 파일을** 검사합니다. 파일 안의
주소가 모두 환경 변수인 이유가 이것입니다 — 배포될 설정과 검사된 설정이 같은
파일이어야 합니다. 45개 항목을 봅니다: 화면, `/api/*`, `/health`, 세션 쿠키,
열려 있는 SSE, v1→v2, v2→v1, 없는 파일의 404, SPA 새로고침, 캐시 헤더, 그리고
돌고 있는 FFmpeg 가 같은 PID 로 살아 있는지. `docker` 와 `systemctl` 은 PATH 에서
가로채 두므로, 호출되기만 해도 실패합니다.

릴리스 스크립트 자체의 단위 테스트는 CI 에서 돕니다:

```bash
npx vitest run --config vitest.tooling.config.ts scripts/deploy-web.test.mjs
```

## 7. 아직 하지 않은 것

이 구조는 **화면만** 서버에서 떼어냅니다. Rust 서버를 무중단으로 배포하는 것은
별개의 문제이고, FFmpeg 가 서버 프로세스의 자식인 한 해결되지 않습니다. 워커를
분리하고 `flock` 로 중복 실행을 막는 설계가 선행되어야 합니다.
