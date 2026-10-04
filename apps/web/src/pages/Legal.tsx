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
import { LEGAL_CONTENT } from "./legal-content";

/** Which document a path asks for, or `null` for every other path. */
export function legalPageFor(pathname: string): "terms" | "privacy" | null {
  const path = pathname.replace(/\/+$/, "");
  if (path === "/terms") return "terms";
  if (path === "/privacy") return "privacy";
  return null;
}

export function LegalPage({ which }: { which: "terms" | "privacy" }) {
  const doc = LEGAL_CONTENT[which];
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
