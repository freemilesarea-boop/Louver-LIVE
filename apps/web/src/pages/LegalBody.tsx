/**
 * The body of a legal document, shared by the in-app page (`Legal.tsx`) and
 * the prerendered public page (`seo/Marketing.tsx`). Depends on nothing but
 * the content module, so the prerender can import it.
 */
import { EFFECTIVE_DATE, LEGAL_CONTENT, operatorLines } from "./legal-content";

const link = "text-ink-100 underline underline-offset-2 hover:text-ink-300 break-words";

export function LegalBody({ which }: { which: "terms" | "privacy" }) {
  const doc = LEGAL_CONTENT[which];
  return (
    <div className="text-sm leading-relaxed text-ink-100">
      <p className="text-xs text-ink-500">시행일: {EFFECTIVE_DATE}</p>
      <p className="mt-3 text-ink-300">{doc.summary}</p>

      {doc.sections.map((s) => (
        <section key={s.heading} className="mt-6">
          <h2 className="text-base font-semibold text-ink-100">{s.heading}</h2>
          {s.paragraphs?.map((p) => (
            <p key={p} className="mt-2">
              {p}
            </p>
          ))}
          {s.items && (
            <ul className="mt-2 list-disc space-y-1 pl-5">
              {s.items.map((i) => (
                <li key={i}>{i}</li>
              ))}
            </ul>
          )}
          {s.links && (
            <ul className="mt-2 space-y-1">
              {s.links.map((l) => (
                <li key={l.href + l.label}>
                  <a href={l.href} target="_blank" rel="noopener noreferrer" className={link}>
                    {l.label}
                  </a>
                </li>
              ))}
            </ul>
          )}
        </section>
      ))}

      <section className="mt-6 rounded-md border border-ink-800 px-4 py-3">
        <h2 className="text-sm font-semibold text-ink-100">운영자 정보</h2>
        <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
          {operatorLines().map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-ink-500">{k}</dt>
              <dd className="break-words text-ink-100">
                {k === "이메일" ? (
                  <a href={`mailto:${v}`} className={link}>
                    {v}
                  </a>
                ) : (
                  v
                )}
              </dd>
            </div>
          ))}
        </dl>
      </section>
    </div>
  );
}
