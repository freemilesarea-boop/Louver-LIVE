/**
 * The two documents signup links to, inside the signed-out app frame.
 * The words live in `legal-content.ts` and are rendered by `LegalBody`, the
 * same component the prerendered public pages use.
 */
import { Card } from "@/components/ui";
import { Wordmark } from "../App";
import { LEGAL_CONTENT } from "./legal-content";
import { LegalBody } from "./LegalBody";

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
          <LegalBody which={which} />
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
