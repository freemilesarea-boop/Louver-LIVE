/**
 * The text of the two documents signup links to, as data.
 *
 * Separated from `Legal.tsx` so that the build-time prerender can render these
 * pages into static HTML without importing the signed-in application. The page
 * component and the prerender therefore show the same words.
 *
 * These are **placeholders, and the page says so.** See `Legal.tsx`.
 */
export const LEGAL_CONTENT = {
  terms: {
    title: "이용약관",
    summary:
      "247streams는 업로드한 영상을 클라우드 서버에서 YouTube로 송출하는 서비스입니다. 서비스 이용에 관한 정식 약관은 아래 항목을 포함하여 준비 중입니다.",
    points: [
      "서비스의 범위와 제공 방식 (클라우드 송출, 플레이리스트, 예약)",
      "요금제와 동시 송출 한도",
      "이용자가 업로드하는 영상에 대한 권리와 책임",
      "서비스 중단·장애 시의 처리",
      "계정 해지와 데이터 삭제",
    ],
  },
  privacy: {
    title: "개인정보처리방침",
    summary:
      "247streams가 지금 실제로 저장하는 정보는 다음과 같습니다. 정식 처리방침 문서는 준비 중이며, 아래 내용은 현재 구현 기준의 사실 관계입니다.",
    points: [
      "계정: 이름, 이메일, 비밀번호 해시 (비밀번호 자체는 저장하지 않습니다)",
      "약관 동의 시각",
      "업로드한 영상 파일과 그 메타데이터",
      "송출 대상 정보 — 스트림 키는 암호화하여 보관하며 화면에 다시 표시하지 않습니다",
      "YouTube 계정을 연결한 경우: 채널 정보와 암호화된 토큰",
      "송출 기록 (시작·종료 시각, 전송량, 오류)",
    ],
  },
} as const;
