# 릴리스 만드는 법

Louver Live는 네 가지 설치 파일로 나갑니다.

| 운영체제 | 만들어지는 파일 | 빌드하는 러너 |
| --- | --- | --- |
| Windows 10/11 (64비트) | `.exe` (NSIS), `.msi` | `windows-latest` |
| Mac — M1 이상 (Apple Silicon) | `.dmg` | `macos-14` (arm64) |
| Mac — 2020년 이전 (Intel) | `.dmg` | `macos-13` (x86_64) |
| Linux (x86_64) | `.deb`, `.AppImage` | `ubuntu-22.04` |

macOS 빌드가 두 개인 이유는 하나로 합칠 수 없어서가 아니라, Apple Silicon과
Intel은 서로 다른 기계어이기 때문입니다. Intel용 `.dmg`는 Rosetta를 통해 M1
이상에서도 돌아가지만 느립니다 — 각자 자기 것을 받는 편이 낫습니다.

Linux를 `ubuntu-22.04`에서 빌드하는 것은 의도적입니다. glibc는 앞으로만
호환되므로, 오래된 배포판에서 빌드한 `.deb`는 최신 배포판에서도 설치되지만
그 반대는 안 됩니다.

---

## 릴리스 절차

### 1. 버전을 올립니다

세 곳이 같아야 합니다.

```bash
# Cargo.toml (workspace.package.version) 과 tauri.conf.json (version)
grep -n '^version' Cargo.toml
grep -n '"version"' apps/desktop/src-tauri/tauri.conf.json
grep -n '"version"' package.json
```

### 2. 태그를 밀면 빌드가 시작됩니다

```bash
git tag v1.0.1
git push origin v1.0.1
```

네 개 러너가 동시에 돌고, 각자 이 순서를 지킵니다.

1. FFmpeg 사이드카를 **내려받습니다** (`--require-download --force`).
   내려받지 못하면 그 자리에서 실패합니다 — 러너 자신의 FFmpeg를 대신
   넣지 않습니다. 그 FFmpeg는 동적 링크라 사용자 컴퓨터에서 실행되지
   않습니다.
2. 사이드카를 검사합니다 (라이선스·링크 방식·인코더·프로토콜·최소 버전).
3. 사이드카가 **이 플랫폼용 바이너리가 맞는지** 헤더를 직접 읽어 확인하고,
   출처 기록이 개발용 대체본이 아닌지 확인합니다
   (`node scripts/check-sidecars.mjs --target <triple>`).
4. `npm run verify` 를 **그 플랫폼에서** 돌립니다.
5. 설치 파일을 만듭니다.
6. 빌드된 바이너리에 OAuth 클라이언트가 실제로 들어갔는지 물어봅니다
   (`--credential-check`, 값은 절대 출력하지 않고 있음/없음만).
7. 설치 파일을 모읍니다. 하나도 안 만들어졌으면 실패합니다.

1~3번은 `.github/actions/sidecars` 하나에 들어 있고 **평소 CI도 네 플랫폼
모두에서 같은 것을 돌립니다**. v1.0.0 태그가 macOS와 Windows에서 깨진 이유가
정확히 이 단계들이 릴리스에서만 돌았기 때문입니다 — 태그를 밀기 전에
CI의 `sidecars (…)` 네 개가 초록인지 먼저 보세요.

네 개가 모두 끝나면 **초안(draft) GitHub Release**가 만들어지고 설치 파일이
모두 붙습니다. 초안인 이유는 아래 "아직 확인되지 않은 것"을 사람이 직접
판단한 뒤 공개하라는 뜻입니다. GitHub의 Releases 탭에서 **Publish release**를
누르면 공개됩니다.

### 3. 태그를 밀 권한이 없을 때

Actions 탭 → **Release artifacts** → **Run workflow** 에서

- `publish` 를 **켜고**
- `tag` 에 `v1.0.1` 처럼 원하는 태그를 적으면

같은 네 개 빌드가 돌고 그 태그로 초안 Release가 만들어집니다. **초안
Release는 공개(Publish)하는 순간 그 태그를 직접 만듭니다** — `git push
origin v1.0.1` 이 필요 없습니다.

### 4. 시험 삼아 돌려보려면

같은 화면에서 `publish`를 끄고 실행합니다. 설치 파일이 30일짜리 run
artifact로만 남고 Release는 만들어지지 않습니다.

---

## 필요한 GitHub Secrets

`Settings → Secrets and variables → Actions` 에서 넣습니다.
**없어도 빌드는 성공합니다.** 다만 결과물이 달라집니다.

| Secret | 없으면 어떻게 되나 |
| --- | --- |
| `LOUVER_GOOGLE_CLIENT_ID` | **YouTube 연결 버튼이 동작하지 않습니다.** 앱은 "이 빌드에는 Louver Live의 YouTube 클라이언트가 포함되어 있지 않습니다"라고 말합니다. 스트림 키만으로 하는 방송은 정상입니다 |
| `LOUVER_GOOGLE_CLIENT_SECRET` | 위와 같습니다. Google이 이 데스크톱 클라이언트의 토큰 교환에 시크릿을 요구하므로 **둘 다** 필요합니다 |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | macOS 빌드가 **서명·공증되지 않습니다.** 사용자가 앱을 열면 "확인되지 않은 개발자" 경고가 뜨고, 제어를 누른 채 열기로 우회해야 합니다 |
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | Windows 빌드가 서명되지 않습니다. SmartScreen이 "PC 보호됨"을 띄우고 **추가 정보 → 실행**을 눌러야 합니다 |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 자동 업데이트 번들이 서명되지 않습니다. 업데이터는 현재 꺼져 있으므로(`tauri.conf.json`의 `updater.active: false`) 지금은 영향이 없습니다 |

만드는 방법은 `SIGNING.md`에 있습니다. Google OAuth 클라이언트는
`YOUTUBE_OAUTH_PRODUCTION.md`에 있습니다.

### 빌드에 OAuth 클라이언트가 실제로 들어갔는지 확인하기

Secret이 등록되어 있다는 것과, 그 값이 **컴파일된 바이너리 안에 들어갔다는 것**은
다른 이야기입니다. `option_env!`는 `louver-core`가 컴파일되는 순간에 결정되므로,
secret 없이 만들어진 빌드는 다른 모든 검사를 통과하면서도 YouTube 연결 버튼만
동작하지 않습니다.

그래서 릴리스 워크플로는 방금 만든 바이너리에게 직접 물어봅니다:

```bash
# 환경변수를 전부 비운 상태로 실행합니다 — 사용자가 Finder에서
# 더블클릭했을 때와 같은 조건입니다.
env -i "/Applications/Louver Live.app/Contents/MacOS/louver-desktop" --credential-check
```

```
OAuth Client ID: configured
OAuth Client Secret: configured
```

`missing`이 나오면 그 빌드는 배포하면 안 됩니다. 이 명령은 **값이 아니라 유무만**
출력하므로 고객에게 실행을 부탁해도 안전합니다. 클라이언트가 없으면 종료 코드가
1이라, 워크플로는 출력을 해석하지 않고 실패시킵니다.

`BUILD-INFO-<target>.txt`가 설치 파일 옆에 함께 올라갑니다. 그 파일의
`signed:` 와 `oauth:` 줄이 이 빌드가 서명됐는지, OAuth 클라이언트를 품고
있는지 알려줍니다 — 값 자체는 들어가지 않습니다.

---

## 서명하지 않고 배포할 때 사용자에게 안내할 말

서명 인증서는 유료이고 발급에 시간이 걸립니다. 서명 없이 먼저 내보낼 수도
있지만, 그러면 사용자가 경고를 직접 넘겨야 합니다.

**Mac**: 앱을 처음 열 때 "확인되지 않은 개발자" 경고가 뜹니다.
응용 프로그램 폴더에서 **Louver Live를 제어(Control) 키를 누른 채 클릭 →
열기 → 열기**. 한 번만 하면 됩니다.

**Windows**: "Windows의 PC 보호" 창이 뜨면 **추가 정보 → 실행**.

70대 사용자에게 이 과정을 설명해야 한다는 뜻이므로, 제품으로 내보낼
계획이라면 서명 인증서를 준비하는 편이 좋습니다.

---

## 아직 확인되지 않은 것

Release가 초안으로 만들어지는 이유입니다. 자세한 내용은
`RELEASE_CANDIDATE_REPORT.md` §15에 있습니다.

- **실제 YouTube 방송이 검증되지 않았습니다.** 소켓 직전까지는 실제 RTMP로
  확인됐지만, 실제 채널로 송출해본 적은 없습니다. 절차는
  `YOUTUBE_SETUP.md` §3의 TEST 15~22입니다.
- **macOS와 Windows가 실행 환경에서 검증되지 않았습니다.** 키체인, 자격 증명
  관리자, 절전 방지, 자동 시작, 트레이는 한 번도 실행된 적이 없습니다.
  `MACOS_QA.md` 와 `WINDOWS_QA.md` 가 그 점검표입니다.

초안 Release의 설치 파일을 받아 위 두 가지를 먼저 확인하시고, 그 다음에
**Publish release**를 누르시길 권합니다.
