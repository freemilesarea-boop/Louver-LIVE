/**
 * The public, crawlable pages.
 *
 * These components are rendered twice from the same source: once at build time
 * by `scripts/seo-prerender.mjs`, into static HTML a crawler can read without
 * running any JavaScript, and once in the browser by `App.tsx` when a visitor
 * opens the same URL. That is the whole reason they are kept apart from the
 * rest of the front end:
 *
 *  - nothing here imports the transport, a store, or a page from `../pages`,
 *    so the prerender bundle is these components and React, and nothing else;
 *  - nothing here reads `window`, `document` or a cookie while rendering, so
 *    the server-side render produces the same markup as the browser;
 *  - every word comes from `./content`, so the static HTML and the rendered
 *    page cannot drift apart.
 *
 * The one deliberate difference is the sign-in card on `/`: it needs the
 * transport, so the browser passes it in and the prerender does not. The
 * prerendered home page shows the same marketing copy without the form.
 */
import type { ReactNode } from "react";
import { Wordmark } from "../brand";
import { LEGAL_CONTENT } from "../pages/legal-content";
import {
  PATH_LABELS,
  PLAN_INCLUDES,
  PUBLIC_PAGES,
  PUBLIC_PLANS,
  formatKrw,
} from "./content";
import type { LegalSeo, PublicPage } from "./content";

/* ------------------------------------------------------------------ nav */

function NavLinks({ active }: { active: string }) {
  return (
    <nav
      aria-label="서비스 안내"
      className="flex flex-wrap items-center gap-x-1 gap-y-1"
    >
      {PUBLIC_PAGES.filter((p) => p.path !== "/").map((p) => (
        <a
          key={p.path}
          href={p.path}
          aria-current={p.path === active ? "page" : undefined}
          className={`rounded-md px-2.5 py-1.5 text-sm ${
            p.path === active
              ? "bg-ink-800 text-ink-100"
              : "text-ink-400 hover:text-ink-100"
          }`}
        >
          {PATH_LABELS[p.path]}
        </a>
      ))}
    </nav>
  );
}

function PublicHeader({ active }: { active: string }) {
  return (
    <header className="border-b border-ink-700">
      <div className="mx-auto flex max-w-3xl flex-wrap items-center justify-between gap-x-4 gap-y-2 px-4 py-3">
        <a href="/" className="text-lg" aria-label="247streams 홈">
          <Wordmark />
        </a>
        <NavLinks active={active} />
      </div>
    </header>
  );
}

function PublicFooter() {
  const paths = [
    "/",
    ...PUBLIC_PAGES.filter((p) => p.path !== "/").map((p) => p.path),
    "/terms/",
    "/privacy/",
  ];
  return (
    <footer className="mt-12 border-t border-ink-700 px-4 py-8 text-sm text-ink-400">
      <div className="mx-auto max-w-3xl">
        <nav aria-label="사이트 링크" className="flex flex-wrap gap-x-4 gap-y-2">
          {paths.map((p) => (
            <a key={p} href={p} className="hover:text-ink-100">
              {PATH_LABELS[p]}
            </a>
          ))}
        </nav>
        <p className="mt-4 text-xs text-ink-500">
          247streams — 업로드한 영상을 클라우드 서버에서 YouTube로 송출하는
          서비스입니다.
        </p>
      </div>
    </footer>
  );
}

/** The frame every public page shares. */
export function PublicShell({
  active,
  children,
}: {
  active: string;
  children: ReactNode;
}) {
  return (
    <div className="min-h-screen bg-ink-950 text-ink-100">
      <PublicHeader active={active} />
      <main className="mx-auto max-w-3xl px-4 py-10">{children}</main>
      <PublicFooter />
    </div>
  );
}

/* ---------------------------------------------------------------- pieces */

/** The one call to action. `showPricing` is false on the pricing page itself. */
function Cta({ showPricing }: { showPricing: boolean }) {
  return (
    <div className="mt-8 rounded-lg border border-ink-700 bg-ink-850 p-5">
      <p className="text-sm text-ink-300">
        계정을 만들고 요금제를 선택하면 영상을 올려 방송을 만들 수 있습니다.
      </p>
      <div className="mt-4 flex flex-wrap gap-3">
        {showPricing && (
          <a
            href="/pricing/"
            className="rounded-md bg-ink-100 px-4 py-2 text-sm font-medium text-ink-950 hover:bg-white"
          >
            요금제 보기
          </a>
        )}
        <a
          href="/"
          className={`rounded-md px-4 py-2 text-sm ${
            showPricing
              ? "border border-ink-600 text-ink-100 hover:bg-ink-800"
              : "bg-ink-100 font-medium text-ink-950 hover:bg-white"
          }`}
        >
          로그인 · 회원가입
        </a>
      </div>
    </div>
  );
}

/** The price list, from `PUBLIC_PLANS`. No numbers are written in this file. */
export function PlanCards() {
  return (
    <div className="mt-6 grid gap-4 sm:grid-cols-3">
      {PUBLIC_PLANS.map((p) => (
        <section
          key={p.id}
          className="rounded-lg border border-ink-700 bg-ink-850 p-4"
        >
          <h3 className="text-sm font-semibold uppercase tracking-widest text-ink-400">
            {p.label}
          </h3>
          <p className="mt-2 text-xl font-semibold text-ink-100">
            {formatKrw(p.monthlyKrw)}
            <span className="text-sm font-normal text-ink-400"> / 월</span>
          </p>
          <p className="mt-1 text-xs text-ink-500">{p.who}</p>
          <dl className="mt-4 space-y-1.5 text-sm">
            <div className="flex justify-between gap-2">
              <dt className="text-ink-400">동시 송출</dt>
              <dd className="text-ink-100">{p.concurrent}개</dd>
            </div>
            <div className="flex justify-between gap-2">
              <dt className="text-ink-400">저장 용량</dt>
              <dd className="text-ink-100">{p.storageGb}GB</dd>
            </div>
            <div className="flex justify-between gap-2">
              <dt className="text-ink-400">파일 하나 최대</dt>
              <dd className="text-ink-100">{p.uploadGb}GB</dd>
            </div>
            <div className="flex justify-between gap-2">
              <dt className="text-ink-400">저장 방송 수</dt>
              <dd className="text-ink-100">{p.broadcasts}개</dd>
            </div>
          </dl>
        </section>
      ))}
    </div>
  );
}

function PlanSummary() {
  return (
    <section className="mt-10">
      <h2 className="text-lg font-semibold text-ink-100">요금제</h2>
      <p className="mt-2 text-sm text-ink-400">
        세 요금제의 차이는 동시 송출 수와 저장 용량입니다. 송출 기능은 모든
        요금제에서 같습니다.
      </p>
      <PlanCards />
      <ul className="mt-4 flex flex-wrap gap-x-4 gap-y-1 text-xs text-ink-400">
        {PLAN_INCLUDES.map((f) => (
          <li key={f}>· {f}</li>
        ))}
      </ul>
    </section>
  );
}

function Sections({ page }: { page: PublicPage }) {
  return (
    <>
      {page.sections.map((s) => (
        <section key={s.heading} className="mt-10">
          <h2 className="text-lg font-semibold text-ink-100">{s.heading}</h2>
          {s.body.map((p) => (
            <p key={p} className="mt-3 text-sm leading-relaxed text-ink-300">
              {p}
            </p>
          ))}
          {s.points && (
            <ul className="mt-3 list-disc space-y-1.5 pl-5 text-sm text-ink-300">
              {s.points.map((p) => (
                <li key={p}>{p}</li>
              ))}
            </ul>
          )}
        </section>
      ))}
    </>
  );
}

function FaqList({ page }: { page: PublicPage }) {
  if (page.faq.length === 0) return null;
  return (
    <section className="mt-10">
      <h2 className="text-lg font-semibold text-ink-100">자주 묻는 질문</h2>
      <dl className="mt-3 space-y-5">
        {page.faq.map((f) => (
          <div key={f.q}>
            <dt className="text-sm font-medium text-ink-100">{f.q}</dt>
            <dd className="mt-1.5 text-sm leading-relaxed text-ink-300">
              {f.a}
            </dd>
          </div>
        ))}
      </dl>
    </section>
  );
}

function Related({ page }: { page: PublicPage }) {
  if (page.related.length === 0) return null;
  return (
    <section className="mt-10 border-t border-ink-700 pt-6">
      <h2 className="text-sm font-semibold uppercase tracking-widest text-ink-400">
        이어서 읽기
      </h2>
      <ul className="mt-3 space-y-1.5 text-sm">
        {page.related.map((p) => (
          <li key={p}>
            <a href={p} className="text-ink-100 underline hover:text-white">
              {PATH_LABELS[p]}
            </a>
          </li>
        ))}
      </ul>
    </section>
  );
}

/* ----------------------------------------------------------------- pages */

/**
 * A public page.
 *
 * `signIn` is the browser's sign-in card on `/`. The prerender leaves it out —
 * see the note at the top of this file.
 */
export function MarketingPage({
  page,
  signIn,
}: {
  page: PublicPage;
  signIn?: ReactNode;
}) {
  const home = page.path === "/";
  return (
    <PublicShell active={page.path}>
      <h1 className="text-2xl font-semibold leading-snug text-ink-100 sm:text-3xl">
        {page.h1}
      </h1>
      <p className="mt-4 text-base leading-relaxed text-ink-300">{page.lead}</p>
      {home && signIn ? <div className="mt-8">{signIn}</div> : null}
      {page.path === "/pricing/" && <PlanCards />}
      <Sections page={page} />
      {home && <PlanSummary />}
      <FaqList page={page} />
      {!home && <Cta showPricing={page.path !== "/pricing/"} />}
      <Related page={page} />
    </PublicShell>
  );
}

/** One of the two legal documents, rendered inside the public frame. */
export function LegalArticle({ page }: { page: LegalSeo }) {
  const doc = LEGAL_CONTENT[page.which];
  return (
    <PublicShell active={page.path}>
      <h1 className="text-2xl font-semibold text-ink-100">{doc.title}</h1>
      <p
        role="note"
        className="mt-4 rounded-md border border-warn/40 bg-warn/10 px-3 py-2 text-sm text-warn"
      >
        이 페이지는 준비 중인 초안입니다. 아직 법률 검토를 거친 정식 문서가
        아니며, 정식 문서가 공개되면 이 페이지를 대체합니다.
      </p>
      <p className="mt-4 text-sm leading-relaxed text-ink-300">{doc.summary}</p>
      <ul className="mt-3 list-disc space-y-1.5 pl-5 text-sm text-ink-100">
        {doc.points.map((p) => (
          <li key={p}>{p}</li>
        ))}
      </ul>
    </PublicShell>
  );
}

/**
 * What a URL that is not a page shows.
 *
 * The server answers every unknown path with the single-page shell and a 200 —
 * that is what makes deep links like `/admin/users` work at all. So the honest
 * answer has to be given here, in the page: this view says the URL does not
 * exist and asks the crawler not to index it (`App.tsx` sets the robots meta).
 * It is not an HTTP 404, and nothing in the front end can make it one.
 */
export function PublicNotFound({ path }: { path: string }) {
  return (
    <PublicShell active="">
      <h1 className="text-2xl font-semibold text-ink-100">
        페이지를 찾을 수 없습니다
      </h1>
      <p className="mt-4 text-sm leading-relaxed text-ink-300">
        <span className="break-all font-mono text-ink-400">{path}</span> 주소에
        해당하는 페이지가 없습니다. 아래 안내에서 찾으시는 내용을 확인해
        주세요.
      </p>
      <nav aria-label="주요 페이지" className="mt-6">
        <ul className="space-y-1.5 text-sm">
          {["/", "/youtube-24-live/", "/playlist-live/", "/youtube-live-streaming/", "/pricing/"].map(
            (p) => (
              <li key={p}>
                <a href={p} className="text-ink-100 underline hover:text-white">
                  {PATH_LABELS[p]}
                </a>
              </li>
            ),
          )}
        </ul>
      </nav>
    </PublicShell>
  );
}
