/**
 * The operator console's shell: one layout, one router, seven screens.
 *
 * The router is twenty lines rather than a dependency. The console has eight
 * paths, all of them flat, and the rest of this product already reads
 * `location.pathname` directly for its own two or three cases — adding a router
 * here would put one in the bundle every signed-out visitor downloads so that
 * the handful of people who can open `/admin` get nothing they do not have.
 *
 * Nothing here is a permission check. `App.tsx` decides whether to render this
 * at all, and that decision is a convenience: every `/api/admin/*` route
 * re-reads the caller's row from the database and answers 403 on its own.
 */
import { useEffect, useState } from "react";
import {
  AdminAuditPage,
  AdminBillingPage,
  AdminBroadcastsPage,
  AdminDashboardPage,
  AdminRevenuePage,
  AdminSystemPage,
  AdminUserPage,
  AdminUsersPage,
} from "./pages";

/** The console's root. Everything below is relative to it. */
export const ADMIN_ROOT = "/admin";

/** Is this path the console's? Used by `App.tsx` before it renders anything. */
export function isAdminPath(path: string): boolean {
  const clean = path.replace(/\/+$/, "") || "/";
  return clean === ADMIN_ROOT || clean.startsWith(`${ADMIN_ROOT}/`);
}

const NAV: { path: string; label: string }[] = [
  { path: "/admin", label: "대시보드" },
  { path: "/admin/revenue", label: "매출" },
  { path: "/admin/users", label: "회원" },
  { path: "/admin/broadcasts", label: "방송" },
  { path: "/admin/billing", label: "결제" },
  { path: "/admin/system", label: "시스템" },
  { path: "/admin/audit", label: "감사 로그" },
];

/** Which nav entry a path belongs under — `/admin/users/abc` is still 회원. */
function section(path: string): string {
  const clean = path.replace(/\/+$/, "") || "/";
  if (clean === ADMIN_ROOT) return "/admin";
  const top = clean.split("/").slice(0, 3).join("/");
  return NAV.some((n) => n.path === top) ? top : "/admin";
}

/** The screen a path names, with the id when it carries one. */
function screenFor(path: string, go: (to: string) => void) {
  const clean = path.replace(/\/+$/, "") || "/";
  const parts = clean.split("/").filter(Boolean); // ["admin", …]
  const rest = parts.slice(1);
  if (rest.length === 0) return <AdminDashboardPage go={go} />;
  switch (rest[0]) {
    case "revenue":
      return <AdminRevenuePage />;
    case "users":
      return rest[1] ? (
        <AdminUserPage id={decodeURIComponent(rest[1])} go={go} />
      ) : (
        <AdminUsersPage go={go} />
      );
    case "broadcasts":
      return <AdminBroadcastsPage go={go} />;
    case "billing":
      return <AdminBillingPage go={go} />;
    case "system":
      return <AdminSystemPage />;
    case "audit":
      return <AdminAuditPage />;
    default:
      return <AdminDashboardPage go={go} />;
  }
}

export function AdminApp({ email }: { email: string }) {
  const [path, setPath] = useState(() =>
    typeof window === "undefined" ? ADMIN_ROOT : window.location.pathname,
  );

  // The back button has to work: an operator who opens a member from the list
  // and presses back expects the list, not the dashboard.
  useEffect(() => {
    const onPop = () => setPath(window.location.pathname);
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  const go = (to: string) => {
    if (to === window.location.pathname) return;
    window.history.pushState({}, "", to);
    setPath(to);
    window.scrollTo(0, 0);
  };

  const here = section(path);

  return (
    <div className="min-h-screen bg-ink-950 text-ink-100">
      {/* Wraps rather than overflows, like the member header: at 375px the
          seven sections do not fit on one line, and a header that overflows
          makes the whole document scroll sideways. */}
      <header className="border-b border-ink-700 px-4 py-3 sm:px-6">
        <div className="mx-auto flex max-w-7xl flex-wrap items-center justify-between gap-x-6 gap-y-2">
          <div className="flex min-w-0 flex-wrap items-center gap-x-5 gap-y-2">
            <button
              onClick={() => go(ADMIN_ROOT)}
              className="font-semibold tracking-tight"
            >
              <span className="text-ink-100">247</span>
              <span className="text-ok">streams</span>
              <span className="ml-2 rounded border border-live-dim px-1.5 py-0.5 text-[10px] uppercase tracking-widest text-live">
                admin
              </span>
            </button>
            <nav className="flex flex-wrap gap-1">
              {NAV.map((n) => (
                <button
                  key={n.path}
                  onClick={() => go(n.path)}
                  aria-current={here === n.path ? "page" : undefined}
                  className={`rounded-md px-2.5 py-1.5 text-sm ${
                    here === n.path
                      ? "bg-ink-800 text-ink-100"
                      : "text-ink-400 hover:text-ink-100"
                  }`}
                >
                  {n.label}
                </button>
              ))}
            </nav>
          </div>
          <div className="flex shrink-0 items-center gap-3">
            <span className="max-w-[40vw] truncate text-xs text-ink-500">
              {email}
            </span>
            <a
              href="/"
              className="rounded-md border border-ink-600 px-2.5 py-1 text-xs text-ink-300 hover:bg-ink-800 hover:text-ink-100"
            >
              일반 사용자 페이지로
            </a>
          </div>
        </div>
      </header>
      {/* Wider than the member app on purpose: these screens are tables, and an
          operator reading them wants the columns, not the whitespace. */}
      <main className="mx-auto max-w-7xl p-4 sm:p-6">{screenFor(path, go)}</main>
    </div>
  );
}

/**
 * What a signed-in member sees at `/admin`.
 *
 * Shown rather than hidden, and it tells them nothing: no counts, no names, no
 * hint that a console exists beyond the word. The server would refuse every
 * request this page could make anyway.
 */
export function AdminDenied() {
  return (
    <div className="flex min-h-screen flex-col items-center justify-center gap-4 bg-ink-950 p-6 text-center">
      <p className="text-sm text-ink-300">관리자만 열 수 있는 페이지입니다.</p>
      <a
        href="/"
        className="rounded-md border border-ink-600 px-3 py-2 text-sm text-ink-300 hover:bg-ink-800 hover:text-ink-100"
      >
        247streams로 돌아가기
      </a>
    </div>
  );
}
