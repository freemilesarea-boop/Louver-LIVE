/**
 * The operator console, driven through a stubbed `fetch`.
 *
 * Two things are worth testing here and they are not the tables. The first is
 * that a member who types `/admin` is refused rather than merely unlinked. The
 * second is that the screens show what the server sent and nothing else — no
 * placeholder revenue, no sample rows — because a console that invents numbers
 * is worse than no console.
 */
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { TransportProvider } from "../TransportContext";
import type { Transport } from "../transport";
import type { Me } from "../cloud";
import { AdminApp } from "./AdminApp";
import type { AdminBroadcastRow, AdminDashboard, AdminUserRow } from "./api";

const EMPTY_DASHBOARD: AdminDashboard = {
  users: { total: 0, today: 0, last_7_days: 0, last_30_days: 0, disabled: 0 },
  subscriptions: {
    by_plan: [],
    paid_total: 0,
    unsubscribed: 0,
    new_this_month: 0,
    renewed_this_month: 0,
    cancelled_this_month: 0,
    failed_this_month: 0,
  },
  broadcasts: { running: 0, scheduled: 0, failed: 0, total: 0 },
  revenue: {
    today: 0,
    yesterday: 0,
    this_week: 0,
    this_month: 0,
    last_month: 0,
    this_year: 0,
    all_time: 0,
    month_change_pct: null,
  },
  mrr_krw: 0,
  youtube_accounts: 0,
  storage_bytes: 0,
  disk_free_bytes: 80_000_000_000,
  disk_floor_bytes: 5_000_000_000,
};

const BUSY_DASHBOARD: AdminDashboard = {
  ...EMPTY_DASHBOARD,
  users: { total: 42, today: 3, last_7_days: 9, last_30_days: 20, disabled: 1 },
  subscriptions: {
    by_plan: [
      { plan_id: "basic", label: "Basic", monthly_price_krw: 19900, count: 5 },
      { plan_id: "pro", label: "Pro", monthly_price_krw: 39900, count: 2 },
    ],
    paid_total: 7,
    unsubscribed: 35,
    new_this_month: 4,
    renewed_this_month: 3,
    cancelled_this_month: 1,
    failed_this_month: 0,
  },
  broadcasts: { running: 2, scheduled: 1, failed: 0, total: 11 },
  revenue: {
    today: 19900,
    yesterday: 0,
    this_week: 59700,
    this_month: 179300,
    last_month: 99500,
    this_year: 278800,
    all_time: 278800,
    month_change_pct: 80.2,
  },
  mrr_krw: 179300,
  youtube_accounts: 3,
  storage_bytes: 4 * 1024 ** 3,
};

const USER_ROW: AdminUserRow = {
  id: "u1",
  email: "member@example.com",
  name: "김회원",
  created_at: "2026-09-01 02:00:00",
  role: "user",
  disabled_at: null,
  plan_id: "basic",
  plan_label: "Basic",
  subscription_status: "active",
  billing_status: "active",
  next_charge_at: "2026-10-01",
  storage_bytes: 1024 ** 3,
  storage_limit_bytes: 5 * 1024 ** 3,
  youtube_accounts: 1,
  running_broadcasts: 1,
  total_paid_krw: 39800,
};

const RUNNING: AdminBroadcastRow = {
  id: "b1",
  name: "밤 라디오",
  user_id: "u1",
  email: "member@example.com",
  desired_state: "running",
  runtime_state: "RUNNING",
  restart_count: 0,
  started_at: "2026-09-28 10:00:00",
  stopped_at: null,
  last_heartbeat: "2026-09-29 01:00:00",
  last_error: null,
  scheduled: false,
  loop_forever: true,
  item_count: 3,
  youtube_channel: "밤 라디오 채널",
  youtube_status: "live",
  worker_alive: true,
};

/** Calls the console made, and what each one was answered with. */
type Route = { status?: number; body?: unknown };

function stubFetch(routes: Record<string, Route | undefined>) {
  const calls: { url: string; method: string; body?: string }[] = [];
  const f = vi.fn(async (url: string, init?: RequestInit) => {
    calls.push({
      url,
      method: init?.method ?? "GET",
      body: init?.body as string | undefined,
    });
    // Longest prefix wins, so `/users/u1` can answer differently from `/users`.
    const key = Object.keys(routes)
      .filter((k) => url.startsWith(`/api/admin${k}`))
      .sort((a, b) => b.length - a.length)[0];
    const route = key ? routes[key] : undefined;
    const status = route?.status ?? (route ? 200 : 404);
    return {
      ok: status >= 200 && status < 300,
      status,
      text: async () => JSON.stringify(route?.body ?? { error: "no route" }),
    } as unknown as Response;
  });
  globalThis.fetch = f as unknown as typeof fetch;
  return calls;
}

/** Enough of a transport for `App` to get past its session check. */
function transportFor(me: Me | null): Transport {
  return {
    kind: "web",
    me: vi.fn(async () => {
      if (!me) throw new Error("no session");
      return me;
    }),
    logout: vi.fn(async () => undefined),
    // The member header renders the deployment banner, which asks for this.
    health: vi.fn(async () => ({
      status: "ok",
      version: "1.0.8",
      deployment: "cloud",
      checks: { api: true, database: true, ffmpeg: true, ffmpeg_rtmps: true, storage: true },
    })),
    dashboard: vi.fn(async () => ({ plan_label: "Basic", active: 0, allowed: 1, broadcasts: [] })),
    watchDashboard: vi.fn(() => () => undefined),
    listMedia: vi.fn(async () => []),
    subscription: vi.fn(async () => ({ plan_id: "basic", status: "active" })),
    listDestinations: vi.fn(async () => []),
    listYoutubeAccounts: vi.fn(async () => []),
    youtubeAvailability: vi.fn(async () => ({ configured: false, redirect_uri: "" })),
    billingStatus: vi.fn(async () => ({ configured: false })),
    plans: vi.fn(async () => []),
  } as unknown as Transport;
}

function at(path: string) {
  window.history.replaceState({}, "", path);
}

const realFetch = globalThis.fetch;

beforeEach(() => at("/"));
afterEach(() => {
  globalThis.fetch = realFetch;
  vi.restoreAllMocks();
});

describe("who may open the console", () => {
  it("refuses a signed-in member who types /admin, and asks the server for nothing", async () => {
    const calls = stubFetch({});
    at("/admin");
    render(
      <TransportProvider value={transportFor({ id: "u1", email: "member@example.com", role: "user", plan_id: "basic" })}>
        <App />
      </TransportProvider>,
    );
    expect(await screen.findByText("관리자만 열 수 있는 페이지입니다.")).toBeTruthy();
    // Not merely a hidden menu: the console itself is never rendered.
    expect(screen.queryByText("대시보드")).toBeNull();
    expect(calls).toHaveLength(0);
  });

  it("does not offer the console in the member header for a member", async () => {
    stubFetch({});
    render(
      <TransportProvider value={transportFor({ id: "u1", email: "member@example.com", role: "user", plan_id: "basic" })}>
        <App />
      </TransportProvider>,
    );
    await screen.findByText("member@example.com");
    expect(screen.queryByText("관리자")).toBeNull();
  });

  it("opens the console for an admin and shows the server's own numbers", async () => {
    stubFetch({ "/dashboard": { body: BUSY_DASHBOARD } });
    at("/admin");
    render(
      <TransportProvider value={transportFor({ id: "a1", email: "boss@example.com", role: "admin", plan_id: "business" })}>
        <App />
      </TransportProvider>,
    );
    const today = (await screen.findByText("오늘 매출")).parentElement!;
    expect(within(today).getByText("₩19,900")).toBeTruthy();
    const mrr = screen.getByText("MRR").parentElement!;
    expect(within(mrr).getByText("₩179,300")).toBeTruthy();
    expect(screen.getByText("42")).toBeTruthy(); // 전체 회원
    expect(screen.getByText("신규 4 · 갱신 3")).toBeTruthy();
  });

  it("shows an admin the way into the console from the member header", async () => {
    stubFetch({});
    render(
      <TransportProvider value={transportFor({ id: "a1", email: "boss@example.com", role: "admin", plan_id: "business" })}>
        <App />
      </TransportProvider>,
    );
    const link = await screen.findByText("관리자");
    expect(link.getAttribute("href")).toBe("/admin");
  });
});

describe("the console's screens", () => {
  it("says it is loading, then shows the dashboard", async () => {
    stubFetch({ "/dashboard": { body: EMPTY_DASHBOARD } });
    at("/admin");
    render(<AdminApp email="boss@example.com" />);
    expect(screen.getByText("불러오는 중…")).toBeTruthy();
    expect(await screen.findByText("MRR")).toBeTruthy();
  });

  it("shows zeroes rather than invented sample data on a new install", async () => {
    stubFetch({ "/dashboard": { body: EMPTY_DASHBOARD } });
    at("/admin");
    render(<AdminApp email="boss@example.com" />);
    await screen.findByText("MRR");
    // Four money KPIs, all genuinely zero.
    expect(screen.getAllByText("₩0").length).toBe(4);
    // And no plan rows, because there are no paying members.
    expect(screen.queryByText("Basic")).toBeNull();
  });

  it("says a list is empty rather than filling it", async () => {
    stubFetch({ "/users": { body: [] } });
    at("/admin/users");
    render(<AdminApp email="boss@example.com" />);
    expect(await screen.findByText("조건에 맞는 회원이 없습니다.")).toBeTruthy();
  });

  it("repeats the server's refusal instead of showing a broken page", async () => {
    stubFetch({ "/dashboard": { status: 403, body: {} } });
    at("/admin");
    render(<AdminApp email="boss@example.com" />);
    expect(await screen.findByText("관리자 권한이 필요합니다.")).toBeTruthy();
  });

  it("offers a retry when a screen fails to load, and reloads on it", async () => {
    const calls = stubFetch({ "/dashboard": { status: 500, body: {} } });
    at("/admin");
    render(<AdminApp email="boss@example.com" />);
    await screen.findByText("데이터를 불러오지 못했습니다.");
    await userEvent.click(screen.getByText("다시 시도"));
    await waitFor(() => expect(calls.length).toBe(2));
  });

  it("navigates between sections and puts the section in the URL", async () => {
    const calls = stubFetch({
      "/dashboard": { body: EMPTY_DASHBOARD },
      "/users": { body: [USER_ROW] },
    });
    at("/admin");
    render(<AdminApp email="boss@example.com" />);
    await screen.findByText("MRR");
    await userEvent.click(screen.getByRole("button", { name: "회원" }));
    expect(await screen.findByText("member@example.com")).toBeTruthy();
    expect(window.location.pathname).toBe("/admin/users");
    expect(calls.some((c) => c.url.startsWith("/api/admin/users"))).toBe(true);
  });

  it("opens one member from the list", async () => {
    stubFetch({
      "/users": { body: [USER_ROW] },
      "/users/u1": {
        body: {
          user: USER_ROW,
          media_count: 2,
          payments: [],
          payment_count: 0,
          first_paid_at: null,
          last_paid_at: null,
          billing: [],
          youtube_channels: ["밤 라디오 채널"],
          broadcasts: [],
        },
      },
    });
    at("/admin/users");
    render(<AdminApp email="boss@example.com" />);
    await userEvent.click(await screen.findByText("김회원"));
    expect(window.location.pathname).toBe("/admin/users/u1");
    expect(await screen.findByText(/밤 라디오 채널/)).toBeTruthy();
  });
});

describe("actions ask first", () => {
  it("does not stop a broadcast until the operator confirms", async () => {
    const calls = stubFetch({
      "/broadcasts": { body: [RUNNING] },
      "/broadcasts/b1/stop": { body: { ...RUNNING, desired_state: "stopped", worker_alive: false } },
    });
    at("/admin/broadcasts");
    render(<AdminApp email="boss@example.com" />);
    await userEvent.click(await screen.findByText("강제 종료"));
    // The dialog is up; nothing has been sent.
    expect(screen.getByText(/즉시 중지됩니다/)).toBeTruthy();
    expect(calls.filter((c) => c.method === "POST")).toHaveLength(0);

    await userEvent.click(screen.getByText("취소"));
    expect(calls.filter((c) => c.method === "POST")).toHaveLength(0);

    await userEvent.click(await screen.findByText("강제 종료"));
    await userEvent.type(screen.getByPlaceholderText("예: 고객 요청"), "고객 요청");
    const confirm = screen.getAllByText("강제 종료").find((n) => n.tagName === "BUTTON" && n.className.includes("bg-live"));
    await userEvent.click(confirm!);
    await waitFor(() => {
      const post = calls.find((c) => c.method === "POST");
      expect(post?.url).toBe("/api/admin/broadcasts/b1/stop");
      // The reason travels with the action, because the audit log keeps it.
      expect(JSON.parse(post!.body!)).toEqual({ note: "고객 요청" });
    });
  });
});
