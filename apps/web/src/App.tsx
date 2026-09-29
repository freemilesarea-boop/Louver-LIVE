/**
 * The web app: sign in, then three screens against one account.
 *
 * There is no environment check anywhere below here. The transport was chosen in
 * `main.tsx`, and this file only knows that it has one.
 */
import { useEffect, useState } from "react";
import { Button } from "@/components/ui";
import { CloudDashboard } from "./pages/CloudDashboard";
import { Destinations } from "./pages/Destinations";
import { MediaLibrary } from "./pages/MediaLibrary";
import { Pricing } from "./pages/Pricing";
import {
  BillingComplete,
  BillingPanel,
  isBillingComplete,
} from "./pages/Billing";
import { LegalPage, legalPageFor } from "./pages/Legal";
import { DeploymentBanner, ServerStatus } from "./pages/ServerStatus";
import { SignIn } from "./pages/SignIn";
import { AdminApp, AdminDenied, isAdminPath } from "./admin/AdminApp";
import { useTransport } from "./TransportContext";
import { displayName } from "./cloud";
import type { Me } from "./cloud";

/** The service's name, in one place. */
export function Wordmark({ className = "" }: { className?: string }) {
  return (
    <span className={`font-semibold tracking-tight ${className}`}>
      <span className="text-ink-100">247</span>
      <span className="text-ok">streams</span>
    </span>
  );
}

type Tab = "broadcasts" | "media" | "destinations" | "pricing" | "status";

const TABS: { id: Tab; label: string }[] = [
  { id: "broadcasts", label: "방송" },
  { id: "media", label: "영상" },
  { id: "destinations", label: "송출 대상" },
  { id: "pricing", label: "요금제" },
  { id: "status", label: "서버 상태" },
];

/**
 * What the OAuth callback left in the URL.
 *
 * The callback cannot render anything itself — it is a redirect, because what
 * arrives there is a person in a browser — so it hands the outcome over as two
 * query parameters and this reads them once. The parameters are then removed, so
 * a reload does not repeat the message.
 */
function consentOutcome(): { outcome: string; detail: string } | null {
  if (typeof window === "undefined") return null;
  const q = new URLSearchParams(window.location.search);
  const outcome = q.get("youtube");
  if (!outcome) return null;
  const detail = q.get("detail") ?? "";
  q.delete("youtube");
  q.delete("detail");
  const rest = q.toString();
  window.history.replaceState(
    {},
    "",
    `${window.location.pathname}${rest ? `?${rest}` : ""}`,
  );
  return { outcome, detail };
}

export function App() {
  const t = useTransport();
  // Two static documents signup has to be able to link to. Read once from the
  // URL rather than through a router: two paths are not a routing problem, and
  // adding one would change every screen's import graph for no benefit.
  const [legal] = useState(() =>
    typeof window === "undefined"
      ? null
      : legalPageFor(window.location.pathname),
  );
  // Where PayApp's `returnurl` sends the browser. It proves nothing on its own —
  // the page it renders asks the server what actually happened.
  const [returnedFromPayment, setReturnedFromPayment] = useState(
    () =>
      typeof window !== "undefined" &&
      isBillingComplete(window.location.pathname),
  );
  // `/pricing` opens the tab it names. Still not a router: one more path read
  // once, rather than a dependency every screen would then import.
  const [landedOnPricing] = useState(
    () =>
      typeof window !== "undefined" &&
      window.location.pathname.replace(/\/+$/, "") === "/pricing",
  );
  // The operator console lives under `/admin`. Read once, like the paths above;
  // what it gates on is `me.role`, and the server checks that again on every
  // request the console makes.
  const [wantsAdmin] = useState(
    () => typeof window !== "undefined" && isAdminPath(window.location.pathname),
  );
  const [me, setMe] = useState<Me | null>(null);
  const [checked, setChecked] = useState(false);
  const [consent] = useState(consentOutcome);
  const [tab, setTab] = useState<Tab>(
    consent ? "destinations" : landedOnPricing ? "pricing" : "broadcasts",
  );
  const [notice, setNotice] = useState<{
    outcome: string;
    detail: string;
  } | null>(consent);

  useEffect(() => {
    let live = true;
    // The cookie may already be valid from a previous visit, so ask before
    // showing a sign-in form.
    t.me()
      .then((m) => live && setMe(m))
      .catch(() => undefined)
      .finally(() => live && setChecked(true));
    return () => {
      live = false;
    };
  }, [t]);

  // Before the session check, so the terms are readable without an account —
  // which is the whole point of linking to them from the signup form.
  if (legal) {
    return <LegalPage which={legal} />;
  }

  if (!checked) {
    return (
      <div className="flex min-h-screen items-center justify-center text-sm text-ink-500">
        불러오는 중…
      </div>
    );
  }

  if (!me) {
    return (
      <>
        <DeploymentBanner />
        <SignIn onSignedIn={setMe} />
      </>
    );
  }

  // Answered here rather than by hiding a menu item: `/admin` is a URL anyone
  // can type. A member who types it is told no, and the console is not rendered
  // for them at all — and even if it were, every route it calls is refused.
  if (wantsAdmin) {
    return me.role === "admin" ? <AdminApp email={me.email} /> : <AdminDenied />;
  }

  // After the sign-in gate, unlike the legal pages: knowing whether a payment
  // landed means knowing whose payment it was.
  if (returnedFromPayment) {
    return (
      <BillingComplete
        onDone={() => {
          // Back into the app, without the return path left in the URL. The flag
          // has to be cleared as well: it is read once from the path, so leaving
          // it set keeps this page on screen however the URL is rewritten.
          window.history.replaceState({}, "", "/");
          setReturnedFromPayment(false);
          setTab("broadcasts");
        }}
      />
    );
  }

  return (
    <div className="min-h-screen bg-ink-950 text-ink-100">
      <DeploymentBanner />
      {/* Wraps rather than overflows: at 375px the tabs and the sign-out button
          do not fit on one line, and a header that overflows takes the whole
          page with it — the document becomes wider than the screen and every
          page scrolls sideways. */}
      <header className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-b border-ink-700 px-4 py-3 sm:px-6">
        <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-2 sm:gap-6">
          <Wordmark />
          <nav className="flex flex-wrap gap-1">
            {TABS.map((x) => (
              <button
                key={x.id}
                onClick={() => setTab(x.id)}
                aria-current={tab === x.id ? "page" : undefined}
                className={`rounded-md px-3 py-1.5 text-sm ${
                  tab === x.id
                    ? "bg-ink-800 text-ink-100"
                    : "text-ink-400 hover:text-ink-100"
                }`}
              >
                {x.label}
              </button>
            ))}
          </nav>
        </div>
        <div className="flex shrink-0 items-center gap-3">
          {/* The name when there is one, the email for every account made
              before signup asked for one. Rendered as text by React, so a name
              containing markup is shown, never run. */}
          <span className="max-w-[40vw] truncate text-xs text-ink-500">
            {displayName(me)}
          </span>
          {/* Only offered to an account the server already called an admin.
              Hiding it is not what keeps members out — refusing the requests is. */}
          {me.role === "admin" && (
            <a
              href="/admin"
              className="rounded-md border border-live-dim px-2.5 py-1 text-xs text-live hover:bg-ink-800"
            >
              관리자
            </a>
          )}
          <Button
            size="sm"
            onClick={async () => {
              await t.logout();
              setMe(null);
            }}
          >
            로그아웃
          </Button>
        </div>
      </header>
      <main className="mx-auto max-w-4xl p-4 sm:p-6">
        {notice && (
          <div
            role="status"
            className={`mb-4 flex items-start justify-between gap-4 rounded-md border px-4 py-3 text-sm ${
              notice.outcome === "connected"
                ? "border-ok/40 bg-ok/10 text-ok"
                : "border-live/40 bg-live/10 text-live"
            }`}
          >
            <span>
              {notice.outcome === "connected"
                ? `YouTube 계정을 연결했습니다${notice.detail ? `: ${notice.detail}` : ""}`
                : `YouTube 계정을 연결하지 못했습니다${notice.detail ? `: ${notice.detail}` : ""}`}
            </span>
            <button
              onClick={() => setNotice(null)}
              aria-label="알림 닫기"
              className="text-ink-400"
            >
              ✕
            </button>
          </div>
        )}
        {tab === "broadcasts" && (
          <CloudDashboard onSeePricing={() => setTab("pricing")} />
        )}
        {tab === "media" && <MediaLibrary />}
        {tab === "destinations" && <Destinations />}
        {tab === "pricing" && (
          <div className="space-y-6">
            {/* The account's own subscription first: somebody opening this tab
                usually wants to know what they are on, not what is on offer. */}
            <BillingPanel />
            <Pricing onClose={() => setTab("broadcasts")} />
          </div>
        )}
        {tab === "status" && <ServerStatus />}
      </main>
    </div>
  );
}
