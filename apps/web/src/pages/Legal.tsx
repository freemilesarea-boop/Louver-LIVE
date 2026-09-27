/**
 * The two documents signup links to.
 *
 * These are **placeholders, and say so on the page.** Signup asks people to
 * agree to a named document, so the link has to lead somewhere — a 404 under a
 * required checkbox is worse than an honest "not written yet". What it must not
 * do is read like a finished agreement: nothing here has been drafted or
 * reviewed by anyone qualified to draft it, and a page that pretended otherwise
 * would be a liability rather than a stopgap.
 *
 * TODO(legal): replace both bodies with reviewed documents before charging
 * anybody money, and bump `TERMS_VERSION` in `apps/server/src/auth.rs` when they
 * land so that existing accounts can be asked to agree again.
 */
import { Card } from "@/components/ui";
import { Wordmark } from "../App";

/** Which document a path asks for, or `null` for every other path. */
export function legalPageFor(pathname: string): "terms" | "privacy" | null {
  const path = pathname.replace(/\/+$/, "");
  if (path === "/terms") return "terms";
  if (path === "/privacy") return "privacy";
  return null;
}

const CONTENT = {
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

export function LegalPage({ which }: { which: "terms" | "privacy" }) {
  const doc = CONTENT[which];
  return (
    <div className="min-h-screen bg-ink-950 p-6 text-ink-100">
      <div className="mx-auto max-w-2xl">
        <h1 className="mb-6 text-center text-2xl">
          <Wordmark />
        </h1>
        <Card title={doc.title}>
          {/* Said before anything else, because somebody arriving from the
              signup checkbox has to know what they are looking at. */}
          <p
            role="note"
            className="mb-4 rounded-md border border-warn/40 bg-warn/10 px-3 py-2 text-sm text-warn"
          >
            이 페이지는 준비 중인 초안입니다. 아직 법률 검토를 거친 정식 문서가
            아니며, 정식 문서가 공개되면 이 페이지를 대체합니다.
          </p>
          <p className="text-sm text-ink-400">{doc.summary}</p>
          <ul className="mt-3 list-disc space-y-1 pl-5 text-sm text-ink-100">
            {doc.points.map((p) => (
              <li key={p}>{p}</li>
            ))}
          </ul>
          <p className="mt-4 text-xs text-ink-500">
            문의: <span className="font-mono">247streams</span> 운영자에게
            연락해 주세요.
          </p>
        </Card>
        <p className="mt-4 text-center text-xs text-ink-500">
          <a href="/" className="underline hover:text-ink-100">
            247streams로 돌아가기
          </a>
        </p>
      </div>
    </div>
  );
}
