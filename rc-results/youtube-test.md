# 실제 YouTube 연동 테스트 결과

> 아직 실행되지 않았습니다. 아래 표는 Mac에서 직접 돌리면서 채웁니다.
> **추측으로 PASS를 적지 않습니다.** 하지 않은 항목은 NOT TESTED로 둡니다.

테스트 날짜: ____________
Mac / macOS 버전: ____________
채널: ____________
테스트 방송 공개범위: Private / Unlisted (하나 고르기)
커밋: ____________ (`git rev-parse --short HEAD`)

---

## 결과

| # | 항목 | 결과 | 확인한 내용 |
| --- | --- | --- | --- |
| 1 | Google Cloud OAuth 클라이언트 생성 | | 클라이언트 ID 끝 12자만 적기 |
| 2 | Louver Live에서 계정 연결 | | 채널명과 Channel ID가 화면에 표시됨 |
| 3 | 비공개/일부공개 라이브 생성 | | YouTube Studio에서 생성, 스트림 키 확인 |
| 4 | 제목 변경 | | YouTube에서 바뀐 제목 |
| 5 | 설명 변경 | | YouTube에서 바뀐 설명 |
| 6 | 태그 변경 | | **제목·설명·카테고리가 그대로인지 반드시 확인** |
| 7 | 카테고리가 음악(Music)인지 | | YouTube Studio 세부정보 |
| 8 | 자동 채팅 메시지 3개 등록 | | |
| 9 | 실제 채팅에 순서대로 올라옴 | | 올라온 시각 3개 |
| 10 | 방송 Stop | | 봇이 즉시 멈췄는지 (로그 `CHAT_STOPPED`) |
| 11 | 다시 Start | | 새 broadcast id, 로그 `CHAT_CONNECTED` |
| 12 | stale liveChatId 없음 | | 이전 방송 채팅에 메시지가 가지 않았는지 |

결과는 PASS / FAIL / NOT TESTED 중 하나로 적습니다.

---

## 관찰한 것

(잘 된 것, 이상했던 것, 화면이 헷갈렸던 것 — 자유롭게)

## 실패한 항목

| # | 무엇이 | 화면에 나온 오류 코드 | 로그에서 본 것 |
| --- | --- | --- | --- |
| | | | |

## 로그에서 확인할 줄

`~/Library/Application\ Support/LouverLive/logs/app.log` 와 `stream.log`:

```
YOUTUBE_AUTH_CONNECTED
YOUTUBE_METADATA_UPDATED
YOUTUBE_TAGS_UPDATED
CHAT_CONNECTED
CHAT_MESSAGE_SENT
CHAT_STOPPED
```

## 보안 확인 (§9)

테스트가 끝난 뒤:

```bash
# 토큰이나 스트림 키가 로그에 남지 않았는지 — 아무것도 나오면 안 됩니다
grep -rniE "ya29\.|1//[A-Za-z0-9_-]{20,}|GOCSPX" ~/Library/Application\ Support/LouverLive/logs/

# 데이터베이스에도 없는지 — 채널명과 Channel ID만 나와야 합니다
sqlite3 ~/Library/Application\ Support/LouverLive/louver.db \
  "select key, value from settings where key like 'youtube%';"
```

| 확인 | 결과 |
| --- | --- |
| 로그에 토큰 없음 | |
| DB에 토큰 없음 (채널명/ID만) | |
| 키체인에 refresh token 있음 | |
