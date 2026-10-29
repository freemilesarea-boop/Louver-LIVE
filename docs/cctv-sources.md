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

관리자 세션 쿠키를 `LOUVER_COOKIE` 로 넘긴다. **브라우저에서 가져온다** — 개발자도구,
Application, Cookies, `louver_session` 값. 비밀번호를 명령줄에 쓰지 않는 이유는 그것이
셸 히스토리와 화면에 남기 때문이다.

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

LOUVER_COOKIE='louver_session=...' \
  python3 scripts/cctv-import.py --source vdot --input /root/va-cams.json
```

소스 함수는 발행 기관이 스스로 고장났다고 표시한 카메라(`active=false`,
`problem_stream=true`)와 주소가 없는 행을 버린다. 남은 주소는 서버가 폼에 입력된 것과
같은 규칙으로 검증한다 — **목록은 주소 규칙을 우회하는 수단이 아니다.**

### 2) 열어보고, 통과한 것만 공개

```bash
LOUVER_COOKIE='louver_session=...' \
  nohup python3 scripts/cctv-check.py --provider 'VDOT (Virginia)' \
  > /root/va-check.log 2>&1 &

tail -f /root/va-check.log
```

카메라당 약 5초다. 1,700대면 두 시간을 넘기므로 `nohup` 으로 띄운다. 중단해도 괜찮다 —
다시 실행하면 **확인 안 된 것만** 이어서 한다. 확인 전·실패한 카메라는 카탈로그에
남지만 고객에게는 보이지 않고, 사유는 `check_note` 에 남는다.

소스 쪽이 복구된 뒤에는 `--recheck-failed` 로 실패했던 것만 다시 열어본다.

### 3) 다시 수집 (한국은 필수)

주소가 만료되는 피드는 0~2번을 주기적으로 반복한다. 같은 URL 은 중복되지 않고 갱신되며,
**주소가 바뀐 행은 확인 상태가 초기화**되어 다시 열어보기 전에는 고객에게 노출되지 않는다.

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
