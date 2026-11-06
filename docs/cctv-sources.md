# CCTV 영상 소스 — 어디서 가져오고, 무엇이 안 되는가

이 문서는 `cctv_sources` 카탈로그에 무엇을 넣을 수 있는지에 대한 조사 결과다.
2026-10-29 기준. **이 저장소의 빌드 환경은 외부 피드에 접속할 수 없다** — DNS 자체가
막혀 있다. 그래서 주소를 코드에 넣지 않았고, 아래 "확인됨" 표시는 운영 서버에서 실제로
응답을 받아 읽은 것만 가리킨다. 개별 카메라의 확인은 서버가 한다 —
`POST /api/admin/cctv/sources/{id}/check` 가 FFmpeg로 실제로 열어보고, 통과한 카메라만
고객에게 보인다.

## 먼저: 되는 것과 안 되는 것

이 기능이 요구하는 것은 **연속 재생되는 라이브 스트림**(HLS `.m3u8` 등)이다. 공개 교통
카메라의 상당수는 그것이 아니다.

| 지역 | 상태 | 근거 |
|---|---|---|
| **미국** | **가능** — 여러 주가 키 없이 HLS 공개 | 아래 목록 |
| **한국** | **가능** — 단 인증키 필요, 주소 수명 주의 | ITS 개방형 API |
| **영국 (TfL JamCams)** | **불가** | 라이브가 아니라 **약 5분마다 갱신되는 10초짜리 클립**. FFmpeg가 열면 즉시 끝난다 |
| **일본** | **사실상 불가** | 국토교통성·NEXCO 모두 웹페이지로만 공개하고 문서화된 목록 API가 없다. 한 집계 사이트 기준 14,633대 중 HLS는 70대(0%) |
| **프랑스 (파리)** | **불가** | Sytadin의 공개 데이터는 소통 정보이고 영상이 아니다. 카메라는 5~10초 간격 스냅샷이며, 고속도로는 Vinci·APRR·SANEF가 각자 포털을 운영한다 |
| **네덜란드** | **거의 불가** | 공개 625대 중 영상 117대, 나머지는 정지 이미지 |
| **독일·이탈리아·스페인** | **정부 공개 피드 확인 못 함** | 집계 사이트만 있고 원 출처의 공개 스트림을 찾지 못함 |

집계 사이트(opencctv.org, alltrafficcams.com 등)에는 수만 대가 올라와 있지만, 그들은
**남의 스트림을 임베드**하는 것이고 재배포 권리가 불분명하다. 유튜브로 24/7 재송출하는
용도로는 원 출처를 직접 쓰는 편이 안전하다.

## 미국 — 키 없이 공개된 HLS

아래 엔드포인트는 어느 것도 **공식 문서가 아니다** — 커뮤니티 보고이거나, 지도 화면이
쓰는 경로다. 그래서 적재 절차의 0번이 "먼저 응답을 본다"이고, "확인됨"은 운영 서버에서
실제로 받아 읽은 것만 가리킨다. 기관이 경로나 필드를 바꿔도 아무 공지가 없다.

| 주 | 카메라 수(보고) | 목록 엔드포인트 | 스트림 형태 |
|---|---|---|---|
| Delaware (DelDOT) | ~365 **(적재 완료, 349대 통과)** | `https://tmc.deldot.gov/json/videocamera.json` | `https://video.deldot.gov/live/<id>.stream/playlist.m3u8` |
| Virginia (VDOT 511) | ~1,700 **(확인됨)** | `https://511.vdot.virginia.gov/services/511/map/layers/map/cams` | `https://media-sfs<n>.vdotcameras.com/rtplive/<key>/playlist.m3u8` |
| South Carolina (511SC) | ~800 | Iteris CDN 의 공개 GeoJSON | SkyVDN 의 HLS |
| Maryland (CHART) | — | CHART 공개 데이터 피드 | CHART 서버의 플레이리스트 |
| Louisiana (511LA) | — | `https://www.511la.org/help/endpoint/cameras` (문서) | `https://ITSStreamingBR2.dotd.la.gov/public/<cam>.streams/playlist.m3u8` |

Georgia 511 과 NYSDOT 은 **개발자 키가 필요**하다.

## 한국 — ITS 개방형 API

* 개발자센터: `http://openapi.its.go.kr/portal/dev/dev6.do`
* 인증키: its.go.kr 회원가입 → 마이페이지 → 인증키 신청
* 엔드포인트는 두 계열이 공존한다. 구형 `http://openapi.its.go.kr/api/NCCTVInfo`
  (필드 `CCTVType`, `CCTVName`, `CCTVurl`)와 신형 `https://openapi.its.go.kr:9443/cctvInfo`
  (소문자 `cctvtype`, `cctvurl`, `cctvname`, `coordx`, `coordy`). **섞어 쓰지 말 것.**
* 요청 변수: `key`, `ReqType`/`getType`, `type`(`its` 국도 / `ex` 고속도로),
  `minX`·`maxX`·`minY`·`maxY` 범위.
* **`cctvtype` 으로 걸러야 한다**: `1`·`4` = 실시간 HLS(4는 HTTPS), `2`·`5` = mp4 파일,
  `3` = 정지영상. **1 또는 4만** 쓸 것.
* JSON 은 `response.data` 배열로 감싸여 온다.
* **주소 수명**: 한 개인 프로젝트 기록은 영상 주소가 24시간만 유효하다고 적고 있다.
  공식 확인은 못 했지만, 사실이라면 **주기적으로 다시 수집해 교체해야 한다**. 이것이
  카탈로그를 한 번 채우고 끝낼 수 없는 이유다.
* 제공 범위는 고속도로·국도뿐. 지방도·시군도는 없다. **서울 시내 도로는 들어오지
  않는다** — 서울시 TOPIS는 CCTV 영상을 공식 OpenAPI로 제공하지 않고, 안내에
  비영리 활용이 명시돼 있다.
* 출처(국가교통정보센터) 표시가 요구된다.

## 적재 방법

두 개의 스크립트가 한다. **서버에서** 실행한다 — 이 저장소의 빌드 환경은 외부 피드에
접속할 수 없고, 운영 서버에는 `jq`가 설치돼 있지 않다. 둘 다 표준 라이브러리만 쓴다.

| 스크립트 | 하는 일 |
|---|---|
| `scripts/cctv-import.py` | 피드를 카탈로그 네 필드로 바꿔 등록한다 |
| `scripts/cctv-check.py` | 등록된 카메라를 서버가 직접 열어본다. 통과한 것만 고객에게 보인다 |

### 인증

관리자 세션 쿠키를 **파일로** 넘긴다. 기본 경로는 `/root/.louver-cookie`.

```bash
cat > /root/.louver-cookie     # 브라우저의 louver_session 값을 붙여넣고 Enter, Ctrl-D
chmod 600 /root/.louver-cookie
```

값은 브라우저에서 가져온다 — 개발자도구, Application, Cookies, `louver_session`.
파일에는 값만 있어도 되고 `louver_session=값` 전체여도 된다.

**환경변수로 넘기지 않는 이유는 실패해 봤기 때문이다.**
`export LOUVER_COOKIE="louver_session=$C"` 를 쓰면 붙여넣기가 이름 쪽에 떨어질 수
있고, 그러면 서버는 401 밖에 답할 수 없다. 그리고 그 과정에서 살아 있는 세션 토큰이
명령줄에 올라가 셸 히스토리에 남는다. `cat > 파일` 은 토큰을 표준입력으로 받으므로
히스토리에 남지 않는다. 비밀번호를 명령줄에 쓰지 않는 것과 같은 이유다.

### 0) 먼저 응답을 본다

```bash
curl -sS -o /root/va-cams.json \
     -w 'HTTP %{http_code}  %{size_download} bytes\n' \
     https://511.vdot.virginia.gov/services/511/map/layers/map/cams
head -c 1800 /root/va-cams.json
```

`cctv-import.py` 의 각 소스 함수는 **실제로 받아서 읽은 응답**에 맞춰 작성한다. 남의
글에 적힌 필드 이름으로 쓰지 않는다. 틀리면 아무 데도 닿지 않는 주소 수백 개가 들어간다.

### 1) 등록

```bash
# 무엇이 만들어지는지 먼저 본다. 전송하지 않는다.
python3 scripts/cctv-import.py --source vdot --input /root/va-cams.json --dry-run

# 같은 명령에서 --dry-run 만 뺀다.
python3 scripts/cctv-import.py --source vdot --input /root/va-cams.json
```

소스 함수는 발행 기관이 스스로 고장났다고 표시한 카메라(`active=false`,
`problem_stream=true`)와 주소가 없는 행을 버린다. 남은 주소는 서버가 폼에 입력된 것과
같은 규칙으로 검증한다 — **목록은 주소 규칙을 우회하는 수단이 아니다.**

### 2) 열어보고, 통과한 것만 공개

```bash
# 먼저 20대만. 세 시간을 걸기 전에 패턴이 맞는지 본다.
python3 scripts/cctv-check.py --provider 'VDOT (Virginia)' --limit 20

nohup python3 scripts/cctv-check.py --provider 'VDOT (Virginia)' > /root/va-check.log 2>&1 &
tail -f /root/va-check.log
```

카메라당 약 5초다. 1,700대면 두 시간을 넘기므로 `nohup` 으로 띄운다. 중단해도 괜찮다 —
다시 실행하면 **확인 안 된 것만** 이어서 한다. 확인 전·실패한 카메라는 카탈로그에
남지만 고객에게는 보이지 않고, 사유는 `check_note` 에 남는다.

소스 쪽이 복구된 뒤에는 `--recheck-failed` 로 실패했던 것만 다시 열어본다.

확인이 끝나면 쓰고 난 세션을 끝낸다. 토큰은 30일 유효하므로, 한 번 화면에 보인 토큰은
기다리는 것이 아니라 지우는 것이 맞다.

```bash
python3 scripts/cctv-check.py --logout
```

서버에서 그 세션 행을 지우고 쿠키 파일을 삭제한다. 보여준 토큰에 해당하는 세션만
지우므로 다른 사람의 로그인에는 영향이 없다.

### 한국 — 목록은 한국에서, 영상은 서버에서

측정 결과(2026-10-29): `openapi.its.go.kr` 은 **해외 IP 에서 연결이 거부된다**(독일
서버에서 80·9443 모두 타임아웃, 한국에서 HTTP 200). 반면 영상 호스트
`cctvsec.ktict.co.kr` 은 독일 서버에서 응답한다. 그래서 역할을 나눈다.

| 단계 | 어디서 | 왜 |
|---|---|---|
| 목록 받기 (`curl`) | **한국에 있는 기계** | ITS 가 해외 IP 를 막는다 |
| 등록 (`cctv-import.py`) | 아무 데나 | 247streams API 호출일 뿐 |
| 열어보기 (`cctv-check.py`) | 아무 데나 | **FFmpeg 는 서버가 돈다.** 이 스크립트는 `POST /api/admin/cctv/sources/{id}/check` 를 때리는 HTTP 클라이언트다 |
| 실제 송출 | 운영 서버 | |

즉 **한국 IP 가 필요한 건 첫 줄 하나뿐**이다. 한국에 있는 기계 한 대가 전부를 다
할 수 있고, 그 기계는 운영 서버에 ssh 로 들어갈 필요가 없다.

```bash
# 한국에 있는 기계에서. 고속도로와 국도를 따로 받아 제공처로 구분한다.
curl -sS -m 30 -o /tmp/kr-ex.json \
  "https://openapi.its.go.kr:9443/cctvInfo?apiKey=$(cat /tmp/its-key)&type=ex&cctvType=4&getType=json&minX=124&maxX=132&minY=33&maxY=39"

python3 scripts/cctv-import.py --source its --input /tmp/kr-ex.json \
  --provider '국가교통정보센터 (고속도로)' --cookie-file /tmp/louver-cookie
```

`type=its` 로 한 번 더 받아 `--provider '국가교통정보센터 (국도)'` 로 넣는다.

### 3) 다시 수집 (한국은 필수)

주소가 만료되는 피드는 0~2번을 주기적으로 반복한다. 같은 URL 은 중복되지 않고 갱신되며,
**주소가 바뀐 행은 확인 상태가 초기화**되어 다시 열어보기 전에는 고객에게 노출되지 않는다.

#### 주소가 만료될 때 서버가 하는 일

재수집은 **카탈로그**를 고친다. 송출 중인 방송은 그것만으로는 안 고쳐졌다. 세 가지가
동시에 어긋나 있었고, 셋 다 닫았다.

**(1) 방송이 카메라를 따라간다.** `broadcasts.cctv_url` 은 카메라를 고른 순간의 주소
**복사본**이다. 재수집이 `cctv_sources` 행에 새 주소를 써도, FFmpeg 가 실행된 argv 안에는
낡은 문자열이 들어 있고 `Runtime::spawn_now` 는 `last_args` 를 **그대로 다시 실행**한다.
그래서 재접속 백오프가 2 → 5 → 10 → 20 → 60초로 올라간 뒤 **같은 죽은 주소에 영원히 머물렀다.**
방송은 "재접속 중"으로 보이고 채널은 꺼진 채였다.

이제 `broadcasts.cctv_source_id` 가 어느 카탈로그 행에서 고른 것인지 기억한다. 주소는
**START 시점에 그 행에서 다시 읽고**, 돌고 있는 방송의 카메라 주소가 바뀌면 스케줄러
틱(30초)이 알아차려 그 방송 하나만 새 주소로 다시 시작한다. `expire_lapsed` 와 같은
자리인 이유도 같다 — 주소가 만료되는 건 아무 이벤트도 호출하지 않는다.

- 주소가 **안 바뀐** 카메라는 건드리지 않는다. 재시작은 눈에 보이는 중단이다.
- 한 번 옮긴 방송은 다시 옮기지 않는다. 새 주소를 `cctv_url` 에 적는 것이 다음 비교의 기준이다.
- 카탈로그에서 **내린(retire) 카메라**는 방송을 끄지 않는다. 마지막으로 작동한 주소를 그대로 쓴다.
- **손으로 입력한 주소**(`cctv_source_id` 가 없다)는 아무도 갱신하지 않는다. 고객이 입력한
  주소를 서버가 조용히 덮어쓰는 일은 없다. 폼에서 주소를 손으로 고치면 카탈로그 연결이 끊긴다.

**(2) 카탈로그가 시체를 광고하지 않는다.** `checked_ok` 는 만료가 없는 boolean 이고, 그것을
되돌리는 건 주소 **문자열**이 바뀐 것을 본 재수집뿐이다. 즉 **재수집이 안 돌면 아무것도
초기화되지 않는다** — 지난주에 통과한 카메라가 오늘도 "확인된 카메라"로 제공됐다.

그래서 한국 지역에는 신선도 창이 붙었다: **마지막 확인이 36시간보다 오래된 한국 카메라는
고객 목록에서 빠진다.** 하루 한 바퀴(약 3시간)보다 길어서 평소에는 목록이 깜빡이지 않고,
이틀보다 짧아서 재수집이 멈추면 **카메라가 사라지는 것으로 드러난다.** 미국 피드는 토큰 없는
공개 HLS 이고 확인도 손으로 하므로 창을 적용하지 않는다 — 적용하면 목록을 비우기만 한다.

> 재수집이 안 돌고 있으면 36시간 뒤 한국 카메라가 목록에서 전부 사라진다. 이것은 고장이
> 아니라 **의도된 동작**이다. 죽은 주소로 방송을 만들게 하는 것보다 낫다.

**(3) 숨긴 수가 보인다.** `louver-server --diagnose` 의 `카메라` 줄이 전체 대수, 제공 중인
대수, 그리고 확인이 오래돼 숨긴 대수를 지역별로 적는다. 조용히 비어 가는 카탈로그가
멈춘 재수집의 유일한 증상이므로, DB 를 열지 않고 읽을 수 있어야 한다.

```
카메라   : 914대 중 0대 제공  ← 확인이 오래돼 숨긴 카메라 KR 914대 (매일 재수집이 돌고 있는지 확인하세요)
```

### 매일 자동으로 — `scripts/cctv-daily-kr.sh`

0~2번을 하루 한 번 대신 한다. **한국에 있는 기계에서** 돈다. 한 바퀴 약 세 시간이고,
대부분은 서버가 카메라를 한 대씩 열어보는 시간이다.

```bash
scripts/cctv-daily-kr.sh --dry-run   # 받아서 보여주기만 한다
scripts/cctv-daily-kr.sh             # 등록 + 확인까지 한 바퀴
scripts/cctv-daily-kr.sh --no-check  # 등록만
```

| 단계 | 하는 일 |
|---|---|
| 0 | 이번 실행용 세션을 받는다 (로그인 파일이 있으면) |
| 1 | ITS `type=ex` + `type=its` 를 받고 **카메라 수를 센다** |
| 2 | 제공처(고속도로/국도)로 나눠 등록한다 |
| 3 | 확인한다 (914대 × 약 11초) |
| 끝 | 받은 세션을 되돌린다 — 실행이 중간에 죽어도 |

**세션을 어떻게 얻는가.** 두 가지이고, 아무도 안 보는 기계에서는 앞쪽이 낫다.

| | 수명 | 문제 |
|---|---|---|
| `~/.louver-login` (메일/비밀번호 두 줄) | 실행당 하나, 끝나면 폐기 | 비밀번호는 바꾸지 않으면 취소할 수 없다 |
| `~/.louver-cookie` (브라우저에서 복사) | 30일 | 매달 손으로 갈아야 하고, 그때까지 살아 있다 |

둘 다 **계정 전체 권한**이다. 서버에 범위 제한 API 키가 없어서 다른 선택지가 없다 —
있으면 그걸 써야 한다. 비밀번호 쪽을 권하는 이유는 하나뿐이다: *세션*이 실행보다
오래 살지 않는다.

**조용히 성공하는 실패를 막는 부분.**

- HTTP 200 만으로 믿지 않고 **카메라 수를 센다**. 취소된 인증키는 200 에 오류 문서를
  담아 보낸다 — 그대로 등록하면 어제 행이 갱신되지 않은 채 모든 줄이 성공을 보고한다.
  300대 미만이면 멈춘다.
- 걸러낼 낱말이 **등록·확인 한 벌**이다. 어긋나면 등록만 되고 영원히 안 보이는
  카메라나, 어제 주소로 확인되는 카메라가 생긴다.
- `mkdir` 잠금으로 한 번에 하나만 돈다. 두 벌이 겹치면 이미 옮겨간 주소를 검사한다.
- 비밀은 파일로만 받는다. 인수는 프로세스 목록과 히스토리에 남는다.

| 증상 | 뜻 |
|---|---|
| `HTTP 000` | 해외 IP 차단 또는 네트워크. 한국에서 돌리고 있는지 본다 |
| `카메라가 N 대뿐입니다` | 인증키 만료·거부, 또는 응답이 깨짐 |
| `로그인이 HTTP 401` | 메일·비밀번호가 틀렸다 |
| `등록 실패 ... HTTP 401` | 쿠키 만료(30일). 로그인 파일을 쓰면 없어지는 문제다 |
| `이미 실행 중입니다` | 잠금 디렉터리가 남아 있다. 실제로 안 돌면 지운다 |

### 한국 VPS 에 얹기 — `scripts/cctv-vps-setup.sh`

노트북은 잔다. 05:12 에 자고 있으면 그날은 죽은 주소가 깔린 날이 된다. 잠들지 않는
한국 박스 한 대가 이 문제를 끝낸다.

**필요한 사양은 거의 없다.** 한국 IP, python3, 아웃바운드 HTTPS. 그게 전부다 —
FFmpeg 도, DB 도, 인바운드 포트도, 운영 서버로 가는 ssh 키도 없다. 512MB 로 충분하다.

```bash
scp -r scripts root@<VPS>:/tmp/louver-scripts
ssh root@<VPS> 'bash /tmp/louver-scripts/cctv-vps-setup.sh'
```

하는 일: 꾸러미 설치 → 시간대를 `Asia/Seoul` 로 (UTC 05:12 는 서울 14:12 다) →
로그인 셸 없는 시스템 사용자 `louver` → `/opt/louver` 에 스크립트 →
비밀 두 개를 **에코 없이** 받아 `0600` 으로 → systemd 타이머 →
**마지막에 실제로 ITS 를 불러보고 이 IP 가 되는지 알려준다.**

마지막 단계가 핵심이다. ITS 가 해외 IP 를 막는 건 측정됐지만, **한국 클라우드
대역까지 막는지는 측정하지 않았다.** 그래서 돈을 더 쓰기 전 5분 안에 알 수 있게 해
둔다. 안 되면 박스를 바꾸면 되고, 그 전까지는 아무것도 의미가 없다.

`Persistent=true` 가 노트북과의 차이다. 꺼져 있던 동안 놓친 실행을 켜질 때 알아서
한다. 유닛은 `scripts/systemd/` 에 있고, 읽기 전용 루트·홈 차단·권한 상승 금지로
묶어 뒀다 — 이 박스가 가진 건 자격증명 하나이므로, 잃을 것도 그것뿐이어야 한다.

```bash
systemctl start cctv-daily-kr      # 지금 한 바퀴
journalctl -u cctv-daily-kr -f     # 진행
systemctl list-timers cctv-daily-kr.timer
```

맥에서 돌릴 거면 `scripts/launchd/kr.247streams.cctv-daily.plist` 가 그대로 쓰인다.
다만 자는 동안 놓친 날은 놓친 날이다.

## 한국 — 국립공원공단 (경관)

교통 카메라가 아니라 산과 바다를 비추는 18대다. 설악산 대청봉, 지리산 천왕봉·장터목,
덕유산 설천봉, 오대산 두로령, 태백산 천제단, 소백산 연화봉, 무등산 장불재, 주왕산 기암,
북한산 백운대·사패산, 계룡산 천황봉, 치악산 상원사, 내장산 108단풍길, 변산반도 죽막,
태안해안 학암포, 한려해상 각산, 다도해해상 지리청송해변.

```
https://live.knps.or.kr/cctv/hls/<슬러그>.m3u8
```

측정(2026-10-30): 독일 운영 서버에서 **HTTP 200**. ITS 와 달리 해외 IP 를 막지 않고,
**주소에 토큰이 없어 바뀌지 않는다** — 재수집이 필요 없다.

```bash
python3 scripts/cctv-import.py --source knps     # --input 이 필요 없다
python3 scripts/cctv-check.py --provider 국립공원공단 --recheck-all
```

`--source knps` 는 주소와 이름을 표로 들고 있다. 다른 소스와 반대인데, 이유는
`www.knps.or.kr` 가 목록을 JavaScript 로 그리기 때문이다. 카드 markup 에 `data-url` 은
있지만 `stream-title` 은 HTML 에서 비어 있고, 문서에 있는 한국어 이름은 지도 라벨뿐이라
카드와 연결돼 있지 않다. 둘을 맞추려면 문서상의 순서를 세어야 하는데, 공단이 지도를
재배열하면 그 즉시 조용히 어긋난다 — 엉뚱한 이름이 붙은 카메라가 실패보다 나쁘다.

그래서 짝은 한 번 손으로 맞췄고 로마자로 검증했다: `sapesan`=사패산, `gaksan`=각산,
`zchunje`=천제단, `hakampo`=학암포 — 18개 중 14개가 자기 이름과 일치하고 나머지도 카메라
위치와 맞는다. 공단이 카메라를 추가하면 그 페이지에 나타나고, 그때 표를 고치면 된다.

### 같은 이름의 다른 카메라

카탈로그에서 카메라의 정체는 `(지역, 제공처, 이름)` 이다. 주소가 아니다 — 한국
ITS 의 주소는 하루 만료 토큰이라 같은 카메라가 매일 다른 주소로 들어온다.

그래서 피드 안에 **이름이 똑같은 다른 카메라**가 있으면 한 쪽이 다른 쪽을
덮어쓴다. 실제로 있다: VDOT 2쌍, ITS 23쌍. `cctv-import.py` 는 먼저 피드를 한 번
읽어 겹치는 이름을 찾고, **그 그룹 전원**에게 발행 기관의 id 를 붙인다
(`[경부선] 가양대교 #11`). 두 번째로 온 것에만 붙이면 이름이 피드 순서에
의존하게 되고, 순서가 바뀐 날 두 카메라가 이름을 맞바꾼다.

id 는 피드가 주는 것이어야 한다. ITS 는 주소 경로의 카메라 번호(`.../4003/`)가
토큰과 달리 바뀌지 않고, VDOT 는 `properties.id` 가 있다.

### 지역 코드와 제공처

`region` 은 **두 글자 국가 코드**다(`US`, `KR`, `JP`, `GB`). 주 단위가 아니다 — 그래서
Delaware 와 Virginia 는 둘 다 `US` 에 들어가고, 화면의 지역 선택에는 "미국" 하나로
보인다. 둘을 구분하는 것은 `provider` 이고, 카메라 선택 목록에는
`제공처 · 카메라 이름` 으로 나온다. 미국 주를 여러 개 적재하면 이 목록이 수천 줄이
되므로, 그때는 선택 화면에 제공처 필터나 검색이 필요해진다.

## 운영상 한계 — 먼저 읽을 것

* **동시 1개.** CCTV 방송은 두 입력을 합치므로 stream copy 가 불가능하고 반드시 실시간
  인코딩한다. 인코딩 예산 기본값은 `(코어수 / 2).max(1)` 이고 운영 서버는 2코어라
  **동시에 1개만** 돌아간다. Business 플랜의 동시 송출은 3개지만 CCTV 는 1개다.
  이 기능을 여러 고객에게 팔려면 서버 증설이 먼저다.
* **저작권·이용약관.** 공개 데이터라고 재배포가 허용되는 것은 아니다. 유튜브 24/7
  재송출은 "재배포"에 해당할 수 있고 출처 표시 조건이 붙는 경우가 많다. 소스를
  공개하기 전에 해당 기관의 이용약관을 확인할 것. `provider` 필드가 출처 표시용이다.
* **DNS 리바인딩은 막지 못한다.** `cctv.rs` 주석에 적혀 있다. 확인 시점에 공인 IP 로
  해석되고 연결 시점에 사설 IP 로 바뀌는 호스트는 막히지 않는다.

## 출처

- [국가교통정보센터 OpenAPI 레퍼런스 — CCTV정보](http://openapi.its.go.kr/portal/dev/dev6.do)
- [its.go.kr CCTV 화상자료](https://www.its.go.kr/opendata/opendataList?service=cctv)
- [DelDOT 카메라 수집 사례 (osiris PR #462)](https://github.com/simplifaisoul/osiris/pull/462)
- [Louisiana 511 Cameras API 문서](https://www.511la.org/help/endpoint/cameras)
- [TfL Tech Forum — JamCam 영상은 10초 클립](https://techforum.tfl.gov.uk/t/jamcam-multiple-historic-images/1016)
- [opencctv.org — 일본 카메라 통계](https://opencctv.org/cameras/japan)
- [ARGOS ATLAS — 네덜란드 공개 카메라 625대 중 영상 117대](https://argosatlas.com/en/cameras/netherlands/)
- [graphhopper/open-traffic-collection — 공개 교통 데이터 모음](https://github.com/graphhopper/open-traffic-collection)
- [Cerema — Sytadin 교통 데이터(영상 아님)](https://trafic-routier.data.cerema.fr/sytadin-a105.html)
- [ARGOS ATLAS — 프랑스 카메라는 5~10초 스냅샷, ASFA 분산 운영](https://argosatlas.com/en/blog/como-funcionan-las-camaras-de-trafico/)
- [ARGOS ATLAS — London 839대, 영상 0](https://argosatlas.com/en/cameras/united-kingdom/london/)
- [국토交通省 全国のライブカメラ(웹페이지 공개만)](https://www.mlit.go.jp/road/bosai/LIVEcamera.html)
- [서울시 TOPIS OPEN API 안내](https://topis.seoul.go.kr/refRoom/openRefRoom_4.do)
