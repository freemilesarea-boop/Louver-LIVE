/**
 * The admin console's own client.
 *
 * Deliberately separate from `Transport`. That interface is implemented twice —
 * once for the browser, once for the desktop app over Tauri — and the desktop
 * app has no operator console and never will. Putting these calls there would
 * mean implementing a dozen methods twice so that one of them could throw.
 *
 * Everything below is a read except the two actions, and every response is
 * already free of secrets: the server decides that, not this file.
 */

/** Every money figure in this product is whole won. */
export function won(krw: number): string {
  return `₩${Math.round(krw).toLocaleString("ko-KR")}`;
}

/** Won, shortened, for a chart axis or a tight column. */
export function wonShort(krw: number): string {
  if (krw >= 100_000_000) return `${(krw / 100_000_000).toFixed(1)}억`;
  if (krw >= 10_000) return `${Math.round(krw / 10_000).toLocaleString("ko-KR")}만`;
  return `${krw.toLocaleString("ko-KR")}`;
}

export function bytes(n: number): string {
  if (n >= 1024 ** 4) return `${(n / 1024 ** 4).toFixed(1)}TB`;
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(1)}GB`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(0)}MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)}KB`;
  return `${n}B`;
}

/**
 * A stored UTC timestamp, as the operator's own clock reads it.
 *
 * The database keeps UTC — every comparison in the service depends on that —
 * and 247streams is run from Korea, so the console adds the nine hours on the
 * way to the screen and never the other way round.
 */
export const OFFSET_HOURS = 9;

export function localTime(utc: string | null | undefined): string {
  if (!utc) return "—";
  const at = new Date(`${utc.replace(" ", "T")}Z`);
  if (Number.isNaN(at.getTime())) return utc;
  const shifted = new Date(at.getTime() + OFFSET_HOURS * 3600_000);
  return shifted.toISOString().replace("T", " ").slice(0, 16);
}

export function localDate(utc: string | null | undefined): string {
  return localTime(utc).slice(0, 10);
}

/** `YYYY-MM-DD`, `days` ago, in the operator's timezone. */
export function dayAgo(days: number): string {
  const now = new Date(Date.now() + OFFSET_HOURS * 3600_000 - days * 86_400_000);
  return now.toISOString().slice(0, 10);
}

export interface UserCounts {
  total: number;
  today: number;
  last_7_days: number;
  last_30_days: number;
  disabled: number;
}

export interface PlanCount {
  plan_id: string;
  label: string;
  monthly_price_krw: number;
  count: number;
}

export interface SubscriptionCounts {
  by_plan: PlanCount[];
  paid_total: number;
  unsubscribed: number;
  new_this_month: number;
  renewed_this_month: number;
  cancelled_this_month: number;
  failed_this_month: number;
}

export interface BroadcastCounts {
  running: number;
  scheduled: number;
  failed: number;
  total: number;
}

export interface RevenueSummary {
  today: number;
  yesterday: number;
  this_week: number;
  this_month: number;
  last_month: number;
  this_year: number;
  all_time: number;
  month_change_pct: number | null;
}

export interface GrantCounts {
  active: number;
  /** `[plan_id, label, count]`, in the plans' own order. */
  by_plan: [string, string, number][];
  expiring_7d: number;
  scheduled: number;
}

export interface Grant {
  id: string;
  user_id: string;
  email: string;
  plan_id: string;
  plan_label: string;
  starts_at: string;
  expires_at: string;
  reason: string;
  granted_by_email: string;
  revoked_at: string | null;
  revoked_by_email: string | null;
  revoke_reason: string | null;
  superseded_by: string | null;
  batch_id: string | null;
  created_at: string;
  state: "scheduled" | "active" | "expired" | "revoked";
  days_left: number;
}

export interface GrantOutcome {
  user_id: string;
  email: string;
  action: "created" | "extended" | "reset" | "skipped";
  grant_id: string | null;
  expires_at: string | null;
}

export interface BulkGrantResult {
  batch_id: string;
  plan_id: string;
  plan_label: string;
  outcomes: GrantOutcome[];
}

export interface AdminDashboard {
  /** Entitlement an operator handed out. Never part of any revenue figure. */
  grants: GrantCounts;
  users: UserCounts;
  subscriptions: SubscriptionCounts;
  broadcasts: BroadcastCounts;
  revenue: RevenueSummary;
  mrr_krw: number;
  youtube_accounts: number;
  storage_bytes: number;
  disk_free_bytes: number | null;
  disk_floor_bytes: number;
}

export interface RevenueBucket {
  period: string;
  krw: number;
  payments: number;
  new_krw: number;
  renewal_krw: number;
}

export interface PlanRevenue {
  plan_id: string;
  label: string;
  krw: number;
  payments: number;
}

export interface PaymentRow {
  at: string;
  user_id: string | null;
  email: string | null;
  plan_id: string | null;
  amount_krw: number;
  pay_state: string;
  kind: string;
  is_first: boolean | null;
  pay_type: string | null;
  outcome: string;
  provider_ref: string | null;
}

export interface RevenueReport {
  from: string;
  to: string;
  grain: string;
  summary: RevenueSummary;
  mrr_krw: number;
  active_payers: number;
  arpu_krw: number | null;
  buckets: RevenueBucket[];
  by_plan: PlanRevenue[];
  recent: PaymentRow[];
  reversals: number;
}

export interface AdminUserRow {
  id: string;
  email: string;
  name: string | null;
  created_at: string;
  role: string;
  disabled_at: string | null;
  /** What the account pays for. A grant never changes this. */
  plan_id: string;
  plan_label: string;
  /** The plan actually in force, grant included. */
  effective_plan_id: string;
  effective_plan_label: string;
  entitlement_source: "paid" | "grant" | "none";
  grant_plan_id: string | null;
  grant_expires_at: string | null;
  grant_days_left: number | null;
  subscription_status: string;
  billing_status: string | null;
  next_charge_at: string | null;
  storage_bytes: number;
  storage_limit_bytes: number;
  youtube_accounts: number;
  running_broadcasts: number;
  total_paid_krw: number;
}

export interface AdminBroadcastRow {
  id: string;
  name: string;
  user_id: string;
  email: string;
  desired_state: string;
  runtime_state: string;
  restart_count: number;
  started_at: string | null;
  stopped_at: string | null;
  last_heartbeat: string | null;
  last_error: string | null;
  scheduled: boolean;
  loop_forever: boolean;
  item_count: number;
  youtube_channel: string | null;
  youtube_status: string | null;
  worker_alive: boolean;
}

export interface BillingSubscriptionRow {
  id: string;
  plan_id: string;
  status: string;
  amount_krw: number;
  created_at: string;
  activated_at: string | null;
  cancelled_at: string | null;
  last_paid_at: string | null;
  current_period_end: string | null;
  provider_subscription_id: string | null;
}

export interface AdminUserDetail {
  user: AdminUserRow;
  media_count: number;
  payments: PaymentRow[];
  payment_count: number;
  first_paid_at: string | null;
  last_paid_at: string | null;
  billing: BillingSubscriptionRow[];
  /** Every grant this account has had, newest first. */
  grants: Grant[];
  youtube_channels: string[];
  broadcasts: AdminBroadcastRow[];
}

export interface CancellationRow {
  user_id: string;
  email: string;
  plan_id: string;
  cancelled_at: string | null;
  last_paid_at: string | null;
  amount_krw: number;
  entitlement_plan: string;
  entitlement_active: boolean;
  running_broadcasts: number;
}

export interface BillingMismatch {
  billing_id: string;
  user_id: string;
  email: string;
  plan_id: string;
  billing_status: string;
  cancelled_at: string | null;
  entitlement_status: string;
}

export interface BillingOverview {
  payments: PaymentRow[];
  cancellations: CancellationRow[];
  mismatches: BillingMismatch[];
  configured: boolean;
}

export interface StorageUser {
  user_id: string;
  email: string;
  plan_id: string;
  bytes: number;
  limit_bytes: number;
  files: number;
}

export interface ProblemRow {
  at: string;
  level: string;
  message: string;
  broadcast_id: string;
  broadcast_name: string;
  email: string;
}

export interface SystemView {
  health: {
    status: string;
    version: string;
    deployment: string;
    cookies: string;
    checks: Record<string, boolean>;
  };
  cpu_percent: number;
  memory_used_mb: number;
  memory_total_mb: number;
  disk_free_bytes: number;
  disk_floor_bytes: number;
  storage_bytes: number;
  workers: number;
  running_broadcasts: number;
  storage_leaders: StorageUser[];
  problems: ProblemRow[];
}

export interface AuditRow {
  id: number;
  at: string;
  admin_id: string;
  admin_email: string;
  action: string;
  target_type: string;
  target_id: string;
  before: string | null;
  after: string | null;
  note: string | null;
}

/** What the console says when the server refuses or cannot be reached. */
export class AdminError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`/api/admin${path}`, {
      credentials: "same-origin",
      headers: init?.body ? { "content-type": "application/json" } : {},
      ...init,
    });
  } catch {
    throw new AdminError(0, "서버에 연결할 수 없습니다.");
  }
  const text = await res.text();
  if (!res.ok) {
    let message = "데이터를 불러오지 못했습니다.";
    if (res.status === 401) message = "로그인이 필요합니다.";
    if (res.status === 403) message = "관리자 권한이 필요합니다.";
    try {
      const body = JSON.parse(text) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      /* a proxy's HTML error page; the status is what matters */
    }
    throw new AdminError(res.status, message);
  }
  return (text ? JSON.parse(text) : undefined) as T;
}

const query = (params: Record<string, string | number | undefined>) => {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== "") q.set(k, String(v));
  }
  const s = q.toString();
  return s ? `?${s}` : "";
};

export const adminApi = {
  dashboard: () => call<AdminDashboard>("/dashboard"),
  revenue: (from: string, to: string, grain: string) =>
    call<RevenueReport>(`/revenue${query({ from, to, grain })}`),
  users: (p: { q?: string; filter?: string; sort?: string; limit?: number; offset?: number }) =>
    call<AdminUserRow[]>(`/users${query(p)}`),
  user: (id: string) => call<AdminUserDetail>(`/users/${encodeURIComponent(id)}`),
  broadcasts: (p: { filter?: string; limit?: number }) =>
    call<AdminBroadcastRow[]>(`/broadcasts${query(p)}`),
  billing: (p: { kind?: string; limit?: number }) => call<BillingOverview>(`/billing${query(p)}`),
  system: () => call<SystemView>("/system"),
  audit: (p: { limit?: number; before_id?: number }) => call<AuditRow[]>(`/audit${query(p)}`),
  grants: (p: { filter?: string; limit?: number }) => call<Grant[]>(`/grants${query(p)}`),

  /** Hand a plan to the selected accounts. All of them or none. */
  createGrants: (body: {
    user_ids: string[];
    plan_id: string;
    days?: number;
    from?: string;
    to?: string;
    reason: string;
    on_existing?: "extend" | "reset" | "skip";
  }) => call<BulkGrantResult>("/grants", { method: "POST", body: JSON.stringify(body) }),
  extendGrant: (id: string, days: number, reason: string) =>
    call<Grant>(`/grants/${encodeURIComponent(id)}/extend`, {
      method: "POST",
      body: JSON.stringify({ days, reason }),
    }),
  changeGrantPlan: (id: string, plan_id: string, reason: string) =>
    call<Grant>(`/grants/${encodeURIComponent(id)}/plan`, {
      method: "POST",
      body: JSON.stringify({ plan_id, reason }),
    }),
  revokeGrant: (id: string, reason: string) =>
    call<Grant>(`/grants/${encodeURIComponent(id)}/revoke`, {
      method: "POST",
      body: JSON.stringify({ reason }),
    }),

  setDisabled: (id: string, disabled: boolean, note?: string) =>
    call<AdminUserRow>(`/users/${encodeURIComponent(id)}/disabled`, {
      method: "POST",
      body: JSON.stringify({ disabled, note }),
    }),
  forceStop: (id: string, note?: string) =>
    call<AdminBroadcastRow>(`/broadcasts/${encodeURIComponent(id)}/stop`, {
      method: "POST",
      body: JSON.stringify({ note }),
    }),
};
