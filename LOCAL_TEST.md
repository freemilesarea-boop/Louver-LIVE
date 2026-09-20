# Louver Live 직접 테스트

## 시작하기

```bash
cd Louver-LIVE
npm install
npm run app
```

끝. 잠시 뒤 Louver Live 창이 열립니다.

첫 실행은 Rust 백엔드를 컴파일하기 때문에 몇 분 걸립니다. 그 다음부터는 몇
초입니다.

`npm run app`이 하는 일:

1. Node, Rust, FFmpeg, ffprobe, 프런트엔드, Tauri를 **실제로 실행해서** 확인
2. 이 Mac의 아키텍처에 맞는 FFmpeg/ffprobe sidecar 준비 (없거나 실행되지
   않으면 다시 받음)
3. 로컬 방송 테스트용 RTMP 수신 서버 시작
4. 프런트엔드 개발 서버와 Tauri 데스크톱 앱 실행

문제가 있으면 무엇을 실행해야 하는지 정확히 알려주고 멈춥니다:

```
  Node      OK       v22.22.2
  Rust      OK       cargo 1.94.1
  Frontend  OK       node_modules present
  Tauri     OK       @tauri-apps/cli installed
  FFmpeg    OK       version 7.1.1
  ffprobe   OK       version 7.1.1

Starting Louver Live…
```

---

## 테스트 순서

### 1. 영상 추가

**플레이리스트** → **플레이리스트 만들기** → 이름 입력 → **영상 추가**로 Mac에
있는 MP4를 고릅니다. Finder에서 창으로 끌어다 놓아도 됩니다.

추가하면 실제 ffprobe가 돌아 해상도·프레임·코덱이 바로 표시됩니다.

```
1. video01.mp4
   12초  1280×720  25fps  h264        최적화 필요
2. video02.mp4
   10초  1920×1080  30fps  h264       송출 준비 완료
```

### 2. 최적화

방송 규격과 다른 영상이 있으면 **방송용으로 최적화** 버튼이 나옵니다. 누르면
필요한 디스크 용량을 먼저 보여주고, 확인하면 실제 FFmpeg가 돌아갑니다.

끝나면 **송출 준비 완료**로 바뀝니다. 원본 파일은 건드리지 않고, 변환 결과는
캐시에 저장되므로 앱을 다시 켜도 다시 변환하지 않습니다.

### 3. 플레이리스트 확인

- 드래그해서 순서 바꾸기
- × 로 목록에서 빼기, 휴지통으로 영상 삭제
- 오른쪽 위에서 **Sequential** / **Shuffle** 선택

전체 개수와 전체 재생시간이 아래에 표시됩니다.

### 4. 로컬 방송 테스트

**대시보드** → **로컬 테스트**.

스트림 키 없이 프로그램이 실제로 어떻게 도는지 볼 수 있습니다. 실제 방송과
같은 경로 — concat → stream copy → FFmpeg → RTMP — 를 그대로 쓰고, 목적지만
`npm run app`이 띄워 둔 로컬 수신 서버입니다.

```
● TEST LIVE
현재 영상   video01.mp4
다음 영상   video02.mp4
전체 방송 시간  00:02:08
STREAM COPY   FFmpeg Running   재연결 0
```

**설정 → 개발자 모드**를 켜면 실제로 무슨 일이 벌어지는지 보입니다.

```
State  LIVE          FFmpeg PID  3867      Reconnect  0
CPU    0.8%          RAM         61 MB     Last progress  0s ago
RTMP   CONNECTED     Sent        0.02 GB   Test sink  로컬 RTMP
```

**FFmpeg 크래시 시뮬레이션**을 누르면 실제로 FFmpeg를 죽입니다. PID가 바뀌면서
스스로 복구되는 것을 그 자리에서 볼 수 있습니다. **로그** 페이지에 그대로
남습니다.

```
개발자 도구: FFmpeg 강제 종료를 실행했습니다
방송이 중단되었습니다. 2초 후 자동으로 다시 연결합니다
방송 엔진을 다시 시작합니다
방송이 시작되었습니다
```

### 5. 스트림 키 입력

**설정 → 송출**에서

- **RTMPS 서버 주소** — YouTube는 `rtmps://a.rtmps.youtube.com/live2` (기본값)
- **YouTube 스트림 키** — YouTube Studio에서 복사해 여기에 직접 입력

키는 macOS 키체인에 저장됩니다. 화면에는 가려져 보이고, 로그·데이터베이스·크래시
리포트 어디에도 남지 않습니다. 터미널에 입력하거나 파일에 적어두지 마세요.

### 6. 비공개 라이브 테스트

YouTube Studio에서 **비공개(Private)** 또는 **일부공개(Unlisted)** 방송을 하나
만들어 두고, 대시보드에서 **방송 시작**을 누릅니다.

방송 전에 preflight가 돌아 플레이리스트·키·네트워크·FFmpeg를 확인하고, 문제가
있으면 무엇이 문제인지 알려줍니다.

30분쯤 두고 보면서 확인할 것:

- YouTube Studio의 스트림 상태가 정상인지
- 영상이 바뀌는 순간 멈칫하거나 검게 되지 않는지
- **FFmpeg CPU**가 한 자릿수에 머무는지 (STREAM COPY가 맞다는 뜻)
- 재연결 횟수가 0인지

중간에 Wi-Fi를 꺼 보면 RECONNECTING → LIVE로 스스로 돌아오는 것을 볼 수
있습니다.

---

## YouTube 방송 정보와 자동 채팅

**방송 설정** 페이지에서 제목·설명·태그·카테고리·공개범위를 입력하고, 자주 쓰는
조합은 프리셋으로 저장해둘 수 있습니다. 태그는 Enter나 쉼표로 추가되고 중복은
자동으로 걸러집니다. 옆의 숫자는 YouTube의 500자 한도를 얼마나 썼는지인데,
공백이 있는 태그는 따옴표 2자를 더 먹습니다.

실제 YouTube에 반영하려면 **설정 → YouTube**에서 계정을 연결해야 합니다. 준비
과정과 테스트 절차는 **[YOUTUBE_SETUP.md](YOUTUBE_SETUP.md)** 에 있습니다.
연결하지 않아도 내용은 저장되고, 영상 송출은 연결과 무관하게 그대로 됩니다.

같은 페이지 아래쪽에서 자동 채팅 메시지를 등록합니다. 방송이 실제 LIVE가 되고
채팅이 열린 뒤에만 전송되며, 최소 간격은 5분입니다.

---

## 예약 방송 테스트

**방송 예약**에서 시작·종료 시각과 요일을 고릅니다. 자정을 넘기는 시간대도
됩니다 (예: 20:00 → 08:00).

개발자 모드를 켜면 **테스트 프리셋**이 나옵니다. 1분 뒤 시작하는 예약을 만들어
바로 확인할 수 있습니다. 20:00까지 기다릴 필요가 없습니다.

---

## 앱을 껐다 켜도 남는 것

창을 닫고 다시 `npm run app`을 해도 그대로입니다.

| | |
| --- | --- |
| 플레이리스트와 순서 | 유지 |
| 영상 정보 | 유지 |
| 최적화 캐시 | 유지 (다시 변환하지 않음) |
| 예약 | 유지 |
| 설정 | 유지 |
| 스트림 키 | 키체인에서 다시 읽음 |

---

## 자주 겪는 것

| 증상 | 원인과 해결 |
| --- | --- |
| `FFmpeg FAILED — present but will not run` | 다른 아키텍처용 바이너리입니다. `npm run app`이 자동으로 다시 받습니다. 안 되면 `node scripts/fetch-ffmpeg.mjs --target aarch64-apple-darwin --force` |
| `Rust FAILED` | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| 첫 실행이 오래 걸림 | Rust 백엔드 컴파일입니다. 두 번째부터는 빠릅니다 |
| 로컬 테스트에서 `Test sink 파일` | 수신 서버 없이 실행했다는 뜻입니다 (`--no-sink`). 그래도 방송 엔진은 똑같이 돕니다 |
| 스트림 키가 재시작하면 사라짐 | 키체인을 못 쓰는 환경입니다. macOS에서는 키체인에 저장됩니다 |

## 그 밖의 명령

```bash
npm run app -- --check      # 검사만 하고 실행하지 않음
npm run app -- --no-sink    # 로컬 수신 서버 없이 실행
npm run verify              # 전체 테스트
```
