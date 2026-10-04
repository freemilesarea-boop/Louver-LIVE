# 247streams 운영 매뉴얼

장애가 났을 때, 그리고 장애가 나기 전에 해야 하는 것. 명령어는 전부 서버에 SSH로
들어가 `~/louver-live` 에서 실행하는 것을 기준으로 합니다.

배포 자체는 [CLOUD_TESTING.md](CLOUD_TESTING.md), 결제는 [PAYAPP.md](PAYAPP.md),
YouTube 는 [YOUTUBE_OAUTH.md](YOUTUBE_OAUTH.md), 매출·회원·방송을 화면으로 보는
관리 콘솔은 [ADMIN.md](ADMIN.md), 디스크에 무엇이 쌓이고 무엇을 지워도 되는지는
[STORAGE.md](STORAGE.md), 공개 페이지와 검색 노출은 [SEO.md](SEO.md) 를 보세요.

---

## 1. 지금 상태가 어떤지

| 보고 싶은 것 | 명령 |
| --- | --- |
| 서비스가 살아 있는지 | `curl -s localhost:8080/health` |
| 컨테이너가 돌고 있는지 | `docker compose ps` |
| 무엇이 송출 중이고 어디로 보내는지 | `docker compose exec louver louver-server --diagnose` |
| 최근 로그 | `docker compose logs --tail 200 louver` |
| 누가 어떤 요금제인지 | `docker compose exec louver louver-server --audit-plans` |
| 해지했는데 권한이 남은 계정 | `docker compose exec louver louver-server --audit-billing` |
| 요금제 저장 한도와 계정별 사용량 | `docker compose exec louver louver-server --audit-storage` |
| 디스크에 실제로 뭐가 있는지 (읽기 전용) | `docker compose exec louver louver-server --audit-media-storage` |
| 이 영상이 왜 이렇게 준비됐는지 | `docker compose exec louver louver-server --media-check <파일>` |
| 매출·회원·방송을 화면으로 | `https://247streams.kr/admin` ([ADMIN.md](ADMIN.md)) |
| 관리자 계정 목록 | `docker compose exec louver louver-server --list-admins` |

`/health` 의 `checks` 는 전부 `true` 여야 합니다. 하나라도 `false` 면 HTTP 503 이
나오고, `status` 는 `degraded` 입니다.

| false 인 항목 | 뜻 | 할 일 |
| --- | --- | --- |
| `database` | SQLite 를 열거나 읽지 못함 | 디스크 여유와 볼륨 마운트 확인 |
| `ffmpeg` | 사이드카가 실행되지 않음 | 이미지를 다시 빌드 |
| `ffmpeg_rtmps` | FFmpeg 에 TLS 가 없음 (YouTube 송출 불가) | 이미지를 다시 빌드 |
| `storage` | 미디어 디렉터리에 쓸 수 없음 | 권한·디스크 확인 |
| `disk` | 여유 공간이 5GB 미만 | **아래 3번** |

`--diagnose` 는 스트림 키, client secret, 토큰을 출력하지 않습니다. 그대로
복사해서 공유해도 됩니다.

---

## 2. 백업

| 무엇 | 왜 | 어떻게 |
| --- | --- | --- |
| `~/louver-live/.env` | master key. 잃으면 **저장된 스트림 키와 YouTube 토큰을 전부 복호화할 수 없습니다** | 아래 |
| `louver-data` 볼륨의 `cloud.db` | 계정·요금제·결제 기록·방송·플레이리스트 | 아래 |
| `louver-data` 볼륨의 `media/` | 업로드 원본과 변환본 | 아래 (용량이 큼) |

이미지는 백업하지 않습니다. master key 는 이미지에 들어 있지 않습니다 — 들어
있다면 레지스트리에 키를 함께 배포하는 셈입니다.

```bash
# 1. master key (작지만 가장 중요합니다. 서버 밖에 두세요)
scp <서버>:louver-live/.env ~/247streams-backup/env-$(date +%F)

# 2. 데이터베이스 — 송출 중에도 안전한 방법 (SQLite 온라인 백업)
ssh <서버> 'cd louver-live && docker compose exec -T louver \
  sqlite3 /var/lib/louver/cloud.db ".backup /var/lib/louver/backup.db"' || \
ssh <서버> 'cd louver-live && docker compose exec -T louver \
  cp /var/lib/louver/cloud.db /var/lib/louver/backup.db'
ssh <서버> 'cd louver-live && docker compose cp louver:/var/lib/louver/backup.db -' \
  > ~/247streams-backup/cloud-$(date +%F).db
ssh <서버> 'cd louver-live && docker compose exec -T louver rm -f /var/lib/louver/backup.db'

# 3. 미디어 (수십 GB 가 될 수 있습니다)
ssh <서버> 'cd louver-live && docker compose exec -T louver tar -cf - -C /var/lib/louver media' \
  > ~/247streams-backup/media-$(date +%F).tar
```

> 이미지에 `sqlite3` 가 없으면 위 첫 줄이 실패하고 `cp` 로 넘어갑니다. WAL 이
> 켜진 DB 를 그냥 복사하면 마지막 몇 초의 쓰기가 빠질 수 있습니다. 계정과 결제
> 기록에 대해서는 허용되는 수준이고, 확실히 하려면 **2번 전에 `docker compose stop
> louver` 로 멈추세요** — 그 동안 방송은 끊깁니다.

**하루 한 번 자동으로:**

```bash
# crontab -e
15 4 * * * cd ~/louver-live && docker compose exec -T louver cp /var/lib/louver/cloud.db /var/lib/louver/daily.db && docker compose cp louver:/var/lib/louver/daily.db ~/backups/cloud-$(date +\%u).db
```

일주일치가 돌아가며 덮어씁니다. `~/backups` 도 서버 밖으로 한 번씩 내려받으세요 —
서버가 사라지면 서버 안의 백업도 사라집니다.

---

## 3. 디스크가 차기 시작할 때

80GB 짜리 서버입니다. 다음 순서로 봅니다.

```bash
df -h /                                    # 전체
docker system df                           # 이미지·빌드 캐시
docker compose exec louver du -sh /var/lib/louver/*   # DB / media / work / uploads
docker compose exec louver louver-server --diagnose | head -8   # 여유 공간 한 줄
```

| 무엇이 차 있는지 | 조치 |
| --- | --- |
| 빌드 캐시·옛 이미지 | `docker system prune -af` (실행 중 컨테이너는 남습니다) |
| 컨테이너 로그 | `max-size: 50m`, `max-file: 3` 으로 제한되어 있습니다. 더 줄이려면 `docker-compose.yml` 의 `logging` 을 수정 |
| `media/` | 수강생에게 쓰지 않는 영상 삭제를 요청, 또는 요금제의 `max_storage_bytes` 를 낮춤 |
| `work/` | 방송별 작업 디렉터리. 삭제된 방송의 것은 자동으로 지워집니다 |
| `uploads/` | 중단된 업로드 조각. 서버가 뜰 때 자동으로 정리됩니다 |

여유가 **5GiB** 아래로 내려가면 **업로드가 거부됩니다**(HTTP 507, 사용자에게는
"현재 서버 저장공간이 부족하여 업로드할 수 없습니다"). 변환이 필요한 파일은 여기에
더해 **예상 결과물 크기**만큼의 여유가 더 있어야 합니다 — 변환이 필요 없는 파일은
결과물을 만들지 않으므로 추가로 요구하지 않습니다. 요금제 저장공간이 남아 있어도
디스크가 부족하면 거부됩니다 — 요금제 한도는 계정별이고, 서버 디스크는 모두가 함께
씁니다. 이미 송출 중인 방송은 계속됩니다. 이 한도가 있는 이유는 디스크가 완전히
차면 데이터베이스도, 로그도, 변환본도 쓸 수 없어 **서비스 전체가 멈추기**
때문입니다. 검사 지점과 예상 크기 계산은 [STORAGE.md](STORAGE.md) 6번.

---

## 4. 복구

### 4-1. 컨테이너가 죽었거나 응답이 없다

```bash
docker compose ps                 # 상태 확인
docker compose logs --tail 100 louver
docker compose restart louver
```

`desired_state=running` 이던 방송은 뜨자마자 자동 복구됩니다(30초 안팎 끊김).
사용자가 중지한 방송은 복구되지 않습니다 — 그것이 의도입니다.

### 4-2. 서버를 새로 만들어 복원한다

**production 을 지우기 전에 반드시 임시 서버에서 한 번 해 보세요.** 아래 절차는
새 서버(또는 임시 서버)에 복원하는 것을 기준으로 씁니다.

```bash
# 1. 코드와 컨테이너를 올린다 (아직 시작하지 않는다)
./scripts/deploy-vps.sh <새-서버>        # 로컬 저장소에서
ssh <새-서버> 'cd louver-live && docker compose stop louver'

# 2. master key 를 원래 것으로 되돌린다  ← 이것을 빠뜨리면 스트림 키가 전부 깨집니다
scp ~/247streams-backup/env-<날짜> <새-서버>:louver-live/.env
ssh <새-서버> 'chmod 600 louver-live/.env'

# 3. 데이터베이스를 넣는다
ssh <새-서버> 'cd louver-live && docker compose run --rm -T --entrypoint sh louver \
  -c "cat > /var/lib/louver/cloud.db"' < ~/247streams-backup/cloud-<날짜>.db

# 4. 미디어를 넣는다
ssh <새-서버> 'cd louver-live && docker compose run --rm -T --entrypoint sh louver \
  -c "tar -xf - -C /var/lib/louver"' < ~/247streams-backup/media-<날짜>.tar

# 5. 시작하고 확인한다
ssh <새-서버> 'cd louver-live && docker compose up -d'
ssh <새-서버> 'cd louver-live && curl -s localhost:8080/health'
ssh <새-서버> 'cd louver-live && docker compose exec louver louver-server --diagnose'
```

**복원이 됐는지 판단하는 기준:**

1. `/health` 의 모든 `checks` 가 `true`
2. `--diagnose` 의 각 방송에 `스트림 키: [REDACTED len=.. sha256:....]` 가 나온다
   — 길이와 지문이 나오면 master key 로 복호화가 된 것입니다. 나오지 않으면 **2번
   단계의 .env 가 원래 것이 아닙니다**
3. 기존 계정으로 로그인이 된다
4. `--audit-plans` 가 기존 요금제를 그대로 보여준다
5. 방송을 하나 시작해 YouTube 에 올라가는지 본다

`.env` 를 잃었다면: 계정·요금제·결제 기록·업로드한 영상은 모두 살아 있고,
**스트림 키와 YouTube 연결만** 복구할 수 없습니다. 수강생에게 YouTube 를 다시
연결하게 하거나 스트림 키를 다시 입력하게 해야 합니다.

### 4-3. 해지했는데 권한이 남은 계정

```bash
docker compose exec louver louver-server --audit-billing              # 읽기만
docker compose exec louver louver-server --audit-billing --fix --yes  # 회수
```

`--fix` 는 위에 출력된 계정만 건드립니다. 부팅 migration 이 요금제를 자동으로
바꾸는 일은 없습니다.

### 4-4. 저장 한도를 새 값으로 맞춘다

베타 한도는 Basic 5GB / Pro 10GB / Business 20GB 입니다. 기존 production 행은
부팅으로 바뀌지 않으므로(운영자가 올려 둔 한도를 되돌리지 않기 위해) 직접
적용해야 합니다.

```bash
docker compose exec louver louver-server --audit-storage               # 읽기만
docker compose exec louver louver-server --audit-storage --apply --yes
```

`--apply` 는 `max_storage_bytes` 와 `max_upload_bytes` 만 씁니다. 월요금과 동시
송출 수는 이 명령으로 바뀌지 않습니다. **파일은 하나도 지우지 않고 방송도 끊지
않습니다.** 새 한도를 이미 넘은 계정은 가진 것을 그대로 유지하고 방송도 계속하며,
영상을 지울 때까지 추가 업로드만 거부됩니다.

### 4-5. 디스크에 남은 파일을 확인한다

```bash
docker compose exec louver louver-server --audit-media-storage
```

어떤 행도 가리키지 않고, manifest 에도 없고, 열려 있지도 않은 파일만 `[정리 가능]`
으로 표시합니다. **이 명령은 아무것도 지우지 않습니다.** 삭제는 목록을 눈으로
확인한 뒤 사람이 합니다. 자세한 절차와 예전 변환본 정리는 [STORAGE.md](STORAGE.md).

### 4-6. 새 계정이 Basic 으로 보인다

```bash
docker compose exec louver louver-server --audit-plans
docker compose exec louver louver-server --audit-plans --revoke-unpaid-grants --yes
```

자세한 배경은 [CLOUD.md](CLOUD.md) 의 "Why a new account might show Basic".

---

## 5. 자주 있는 장애

| 증상 | 가장 흔한 원인 | 확인 |
| --- | --- | --- |
| 로그인이 되는 것 같은데 계속 로그인 화면 | HTTPS 없이 `Secure` 쿠키 | `/health` 의 `cookies`. HTTPS 뒤에 있으면 `LOUVER_COOKIE_SECURE=always` |
| 방송이 `재연결 중` 을 반복 | 스트림 키가 틀렸거나 YouTube 가 받지 않음 | `--diagnose` 의 FFmpeg 로그 줄 |
| 결제했는데 요금제가 안 붙음 | `feedbackurl` 이 서버에 닿지 않음 | [PAYAPP.md](PAYAPP.md) 11번 |
| YouTube 연결이 끊김 | refresh token 이 철회됨 | 수강생이 다시 연결 |
| 업로드가 507 "현재 서버 저장공간이 부족하여…" | 서버 디스크 여유가 5GiB 미만이거나, 변환 결과물이 들어갈 자리가 없음 | **3번** |
| 업로드가 402 "… 플랜 저장공간 …GB를 초과합니다" | 그 계정이 요금제 저장공간을 넘음 | 영상 삭제 안내. 파일은 지워지지 않았습니다 |
| 업로드가 402 "파일 크기가 … 파일당 최대 용량 …" | 파일 하나가 요금제의 파일당 한도를 넘음 | 파일을 나누거나 상위 요금제 |
| 업로드 준비가 오래 걸린다 | 그 파일이 재인코딩 대상 | `--media-check <파일>` 가 이유를 한 줄로 알려줍니다. 서버 로그에도 `media <id>: mode=... reason=...` 가 남습니다 |
| 방송이 "영상 형식이 서로 달라" 로 거부된다 | 플레이리스트에 형식이 다른 영상이 섞임 | 그 영상들을 표준 형식으로 다시 준비하는 중입니다. 미디어 목록에서 준비가 끝나면 시작됩니다 |
| 해지했는데 방송이 계속 나간다 | 해지가 실패했을 가능성 | `--audit-billing` 과 로그. 해지 성공 시에는 방송도 즉시 종료됩니다 |
| 로그인이 429 | 한 IP 에서 1분에 10회 초과 | 1분 기다리면 풀립니다 |

---

## 6. 출시 전 점검

```bash
npm run verify                 # 11단계 전체
node scripts/release-smoke.mjs # 가입→결제→YouTube→송출→관리콘솔→해지 20단계 (가짜 PayApp/Google)
node scripts/mobile-smoke.mjs  # 휴대폰 4개 화면 폭에서 레이아웃
node scripts/capacity-check.mjs # 동시 1·2·3 송출의 CPU/RAM

# 업로드 준비 성능 (Node 없이, production 에서도 그대로)
docker compose exec louver louver-server --media-check /경로/영상.mp4 --compare --run
```

전부 로컬에서 돌고, 실제 PayApp·YouTube 에는 아무 요청도 보내지 않습니다.
