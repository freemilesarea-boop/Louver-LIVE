# CCTV 영상 소스 — 어디서 가져오고, 무엇이 안 되는가

이 문서는 `cctv_sources` 카탈로그에 무엇을 넣을 수 있는지에 대한 조사 결과다.
2026-10-29 기준이며, **이 저장소의 빌드 환경에서는 아래 어느 피드에도 접속할 수 없어
URL을 직접 확인하지 못했다.** 그래서 주소를 코드에 넣지 않았다. 확인은 서버가 한다 —
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
| **일본** | **사실상 불가** | 한 집계 사이트 기준 일본 14,633대 중 HLS는 70대(0%) |
| **네덜란드** | **거의 불가** | 공개 625대 중 영상 117대, 나머지는 정지 이미지 |
| **독일·이탈리아·스페인** | **정부 공개 피드 확인 못 함** | 집계 사이트만 있고 원 출처의 공개 스트림을 찾지 못함 |

집계 사이트(opencctv.org, alltrafficcams.com 등)에는 수만 대가 올라와 있지만, 그들은
**남의 스트림을 임베드**하는 것이고 재배포 권리가 불분명하다. 유튜브로 24/7 재송출하는
용도로는 원 출처를 직접 쓰는 편이 안전하다.

## 미국 — 키 없이 공개된 HLS

아래는 커뮤니티가 보고한 엔드포인트다. **공식 문서가 아니므로 첫 수집 때 필드 이름을
반드시 확인해야 한다.** 그래서 아래 레시피에는 "먼저 한 줄만 찍어보라"는 단계가 들어 있다.

| 주 | 카메라 수(보고) | 목록 엔드포인트 | 스트림 형태 |
|---|---|---|---|
| Delaware (DelDOT) | ~360 | `https://tmc.deldot.gov/json/videocamera.json` | `https://video.deldot.gov/live/<id>.stream/playlist.m3u8` |
| Virginia (VDOT 511) | ~1,700 | 511 지도의 공개 GeoJSON 카메라 레이어 | `vdotcameras.com` 의 HLS |
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
* 제공 범위는 고속도로·국도뿐. 지방도·시군도는 없다.
* 출처(국가교통정보센터) 표시가 요구된다.

## 적재 방법

카탈로그 API 는 `{region, provider, name, url}` 네 필드의 배열만 받는다. 어떤 피드든
`jq` 로 그 모양으로 바꿔 넣으면 된다. **서버에서** 실행한다(이 저장소의 빌드 환경은
외부 피드에 접속할 수 없다).

### 0) 먼저 한 줄만 본다 — 필드 이름 확인

```bash
curl -sS 'https://tmc.deldot.gov/json/videocamera.json' | head -c 2000
```

실제 필드 이름을 확인한 뒤 아래 `jq` 를 그에 맞게 고친다. 추측으로 돌리지 말 것.

### 1) 미국 — Delaware 예시

```bash
COOKIE='louver_session=<운영자 세션 토큰>'
curl -sS 'https://tmc.deldot.gov/json/videocamera.json' \
| jq '[ .[]
        | select(.m3u8s != null and .m3u8s != "")
        | { region: "US",
            provider: "DelDOT (Delaware)",
            name: (.name // .location // .id | tostring),
            url:  (.m3u8s | if type == "array" then .[0] else . end) } ]' \
> /tmp/de.json

curl -sS -X POST https://247streams.kr/api/admin/cctv/sources \
     -H 'content-type: application/json' -H "cookie: $COOKIE" \
     --data @/tmp/de.json | jq '[.[] | select(.ok == false)] | .[0:10]'
```

마지막 줄은 **거부된 행만** 보여준다. 주소 규칙을 통과하지 못한 것들이다.

### 2) 한국 — ITS

```bash
KEY='<발급받은 인증키>'
curl -sS "https://openapi.its.go.kr:9443/cctvInfo?apiKey=$KEY&type=ex&cctvType=4&getType=json&minX=124&maxX=132&minY=33&maxY=39" \
| jq '[ .response.data[]
        | select((.cctvtype | tostring) == "1" or (.cctvtype | tostring) == "4")
        | { region: "KR",
            provider: "국가교통정보센터",
            name: .cctvname,
            url:  .cctvurl } ]' \
> /tmp/kr.json
# 이후 위와 같은 POST
```

`type=ex`(고속도로)와 `type=its`(국도)를 따로 받아 합친다.

### 3) 열어보고, 통과한 것만 공개

등록만으로는 고객에게 보이지 않는다. 서버가 직접 열어봐야 한다.

```bash
# 아직 확인 안 된 카메라 id 목록
curl -sS https://247streams.kr/api/admin/cctv/sources -H "cookie: $COOKIE" \
| jq -r '.[] | select(.checked_at == null) | .id' > /tmp/todo.txt

# 하나씩 확인한다. 한 번에 하나씩인 이유: 매번 남의 서버로 실제 연결이 나간다.
while read -r id; do
  curl -sS -X POST "https://247streams.kr/api/admin/cctv/sources/$id/check" \
       -H "cookie: $COOKIE" | jq -c '{ok, message}'
  sleep 2
done < /tmp/todo.txt
```

확인에 실패한 카메라는 카탈로그에 남지만 고객에게는 보이지 않는다. 사유는
`check_note` 에 남는다.

### 4) 다시 수집 (한국은 필수)

주소가 만료되는 피드는 1·3번을 주기적으로 반복한다. 같은 URL 은 중복되지 않고
갱신되며, **주소가 바뀐 행은 확인 상태가 초기화**되어 다시 열어보기 전에는 고객에게
노출되지 않는다.

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
