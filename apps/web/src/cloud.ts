/**
 * The cloud domain, as the UI sees it.
 *
 * These mirror `louver-cloud`'s serialised models. A stream key is absent by
 * construction: the server sends `key_masked` and there is no field here for a
 * key, so no component can display one and no store can persist one.
 */

export type DesiredState = "stopped" | "running";

/** §2's lifecycle, in the server's own words. */
export type RuntimeState =
  | "CREATED"
  | "PREPARING"
  | "STARTING"
  | "RUNNING"
  | "RECONNECTING"
  | "STOPPING"
  | "STOPPED"
  | "FAILED";

export type MediaState =
  | "uploaded"
  | "analysing"
  | "preparing"
  | "ready"
  | "failed";

export interface Me {
  id: string;
  email: string;
  /** `user` or `admin`. Decides whether the operator console is offered; it
   * never decides whether it works — every admin route re-checks the database. */
  role?: string;
  plan_id: string;
  /** What this person is called. `null` on accounts made before signup asked
   * for a name — every reader falls back to the email. */
  name?: string | null;
}

/** What to call this person in the interface, with a fallback that always works. */
export function displayName(me: Me): string {
  return me.name?.trim() || me.email;
}

/** The floor the server enforces too. Shown on the form so it is not a surprise. */
export const MIN_PASSWORD_CHARS = 10;
/** Matches `louver_cloud::db::MAX_NAME_CHARS`, counted in characters. */
export const MAX_NAME_CHARS = 60;

/**
 * A plan, exactly as the server describes it.
 *
 * The price and the concurrency figure are **not** duplicated anywhere in this
 * app. They arrive from `GET /api/plans`, so what the pricing page shows and
 * what the server charges cannot drift apart.
 */
export interface Plan {
  id: string;
  label: string;
  limits: Record<string, number>;
  monthly_price_krw: number;
  description: string;
  active: boolean;
  sort_order: number;
}

export interface Subscription {
  user_id: string;
  plan_id: string;
  plan_label: string;
  /** `active` or `unsubscribed`. */
  status: string;
  limits: Record<string, number>;
  /** The one question to ask. True only when a paid plan is active. */
  active?: boolean;
  /** `null` when there is no subscription — which is how "no plan" is told from
   * "Basic" without comparing plan ids here. */
  plan?: Plan | null;
}

/** Limit keys, matching `louver_cloud::entitlement`. */
export const MAX_CONCURRENT_STREAMS = "max_concurrent_streams";

/** How many broadcasts a plan may run at once. */
export function concurrentStreams(plan: Plan): number {
  return plan.limits[MAX_CONCURRENT_STREAMS] ?? 0;
}

/** ₩19,900 — whole won, grouped, never a decimal. */
export function formatWon(krw: number): string {
  return `₩${krw.toLocaleString("ko-KR")}`;
}

/**
 * What every paid plan includes.
 *
 * Marketing copy, not entitlements — which is why it lives here and the numbers
 * do not. Nothing branches on this list; it is only ever rendered.
 */
export const PLAN_FEATURES = [
  "24/7 클라우드 송출",
  "YouTube 계정 연결",
  "플레이리스트 방송",
  "예약 송출",
  "PC를 종료해도 계속 송출",
] as const;

/** Where a paid subscription has got to with PayApp. Matches `BillingStatus`. */
export type BillingStatusId =
  | "pending"
  | "registration_failed"
  | "active"
  | "cancel_at_period_end"
  | "cancelled"
  | "payment_failed";

export interface BillingSubscription {
  id: string;
  user_id: string;
  plan_id: string;
  provider: string;
  /** PayApp's own reference. Safe to show; it is what support asks for. */
  provider_subscription_id?: string | null;
  status: BillingStatusId;
  amount_krw: number;
  created_at: string;
  updated_at: string;
  activated_at?: string | null;
  cancelled_at?: string | null;
  last_paid_at?: string | null;
  current_period_end?: string | null;
}

export interface BillingStatusResponse {
  subscription: BillingSubscription | null;
  /** The entitlement's view, which can differ from the provider's. */
  plan: Subscription;
  provider: string;
  /** Can this deployment take a payment at all? */
  configured: boolean;
}

/** What a checkout hands back. The URL is PayApp's, not ours. */
export interface Checkout {
  payurl: string;
  billing_id: string;
  plan_id: string;
  amount_krw: number;
}

/** Korean for each billing state, so no component spells them itself. */
export const BILLING_STATUS_LABELS: Record<BillingStatusId, string> = {
  pending: "결제 대기",
  registration_failed: "등록 실패",
  active: "구독 중",
  cancel_at_period_end: "해지 예약",
  cancelled: "해지됨",
  payment_failed: "결제 실패",
};

/** Is PayApp still going to charge for this, or has it charged already? */
export function billingIsLive(b: BillingSubscription): boolean {
  return ["pending", "active", "payment_failed"].includes(b.status);
}

/** The plan singled out on the pricing page. A presentation choice, nothing more. */
export const RECOMMENDED_PLAN = "pro";

export interface CloudMedia {
  id: string;
  user_id: string;
  filename: string;
  size_bytes: number;
  state: MediaState;
  duration_secs: number;
  width: number;
  height: number;
  fps: number;
  video_codec: string;
  audio_codec?: string | null;
  container: string;
  bitrate_bps: number;
  prepared_duration_secs?: number | null;
  last_error?: string | null;
  created_at: string;
}

export interface StreamDestination {
  id: string;
  user_id: string;
  label: string;
  rtmps_url: string;
  /** Always dots. The key itself never leaves the server. */
  key_masked: string;
  created_at: string;
  /** `manual_rtmps` today. A connected account could carry a title and a
   * privacy to the platform; a pasted key cannot. */
  kind?: DestinationKind;
}

export type Privacy = "public" | "unlisted" | "private";
export type DestinationKind = "manual_rtmps" | "youtube_account";

/** What the sender is asked to produce. `auto` everywhere is the default. */
export interface StreamSettings {
  resolution: "auto" | "720p" | "1080p";
  fps: "auto" | "30" | "60";
  /** 0 = auto. */
  video_bitrate_kbps: number;
  /** 0 = auto. */
  audio_bitrate_kbps: number;
}

export const AUTO_SETTINGS: StreamSettings = {
  resolution: "auto",
  fps: "auto",
  video_bitrate_kbps: 0,
  audio_bitrate_kbps: 0,
};

/** When a broadcast starts by itself. Times are UTC; the UI shows local. */
export interface Schedule {
  enabled: boolean;
  start_at?: string | null;
  stop_at?: string | null;
  timezone: string;
  /** Minutes east of UTC, so a daily repeat keeps the user's clock time. */
  offset_minutes: number;
  /** Monday is bit 0. 0 means once. */
  repeat_days: number;
  last_run_at?: string | null;
}

export function emptySchedule(): Schedule {
  return {
    enabled: false,
    start_at: null,
    stop_at: null,
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    offset_minutes: -new Date().getTimezoneOffset(),
    repeat_days: 0,
    last_run_at: null,
  };
}

/** One video in a broadcast's playlist. */
export interface BroadcastItem {
  id: string;
  broadcast_id: string;
  media_id: string;
  position: number;
  enabled: boolean;
  repeat_count: number;
  filename: string;
  duration_secs: number;
  state: MediaState;
}

/** What the API takes when the playlist is replaced. */
export interface NewItem {
  media_id: string;
  enabled: boolean;
  repeat_count: number;
}

export interface Broadcast {
  id: string;
  user_id: string;
  name: string;
  media_id: string;
  destination_id: string;
  loop_forever: boolean;
  desired_state: DesiredState;
  runtime_state: RuntimeState;
  restart_count: number;
  last_error?: string | null;
  created_at: string;
  started_at?: string | null;
  stopped_at?: string | null;
  last_heartbeat?: string | null;
  bytes_sent: number;
  uptime_secs: number;
  ffmpeg_exit_code?: number | null;
  ffmpeg_pid?: number | null;

  /** 247streams' own broadcast information. Not applied to YouTube — see
   * `DestinationKind`. */
  title: string;
  description: string;
  tags: string;
  category: string;
  privacy: Privacy;

  settings: StreamSettings;
  schedule: Schedule;

  /** Videos in the playlist, as the editor shows them. */
  item_count: number;
  /** Entries in one pass once repeats are expanded. What "2 / 8" counts. */
  play_count: number;
  current_index: number;
  current_item?: string | null;
  next_item?: string | null;
  current_position_secs: number;
  current_duration_secs: number;
  cycle_duration_secs: number;

  /** Where this broadcast's YouTube resources are, when an account is
   * connected. Every field is null for a pasted stream key. §7. */
  youtube: YoutubeLink;
}

/** A broadcast with its playlist, for the screen that edits one. */
export interface BroadcastDetail extends Broadcast {
  items: BroadcastItem[];
}

/** Only the fields being changed need to be sent. */
export interface BroadcastPatch {
  name?: string;
  title?: string;
  description?: string;
  tags?: string;
  category?: string;
  privacy?: Privacy;
  loop_forever?: boolean;
  destination_id?: string;
  settings?: StreamSettings;
  schedule?: Schedule;
}

export interface NewBroadcast {
  name: string;
  /** A pasted-key destination. Empty when `youtube_account_id` is set. */
  destination_id: string;
  /** A connected channel, for a broadcast 247streams creates on YouTube. */
  youtube_account_id?: string;
  items: NewItem[];
  loop_forever: boolean;
  title?: string;
  description?: string;
  tags?: string;
  category?: string;
  privacy?: Privacy;
  settings?: StreamSettings;
  schedule?: Schedule;
}

export interface Dashboard {
  plan_label: string;
  /** How many of this account's broadcasts hold a slot right now. */
  active: number;
  /** How many the plan allows. The server decides this; the UI only shows it. */
  allowed: number;
  broadcasts: Broadcast[];
  /** Is there an active subscription? Optional so a payload from an older
   * server still parses; absent is read as subscribed, which is what every
   * account on such a server is. */
  subscribed?: boolean;
  plan_id?: string;
}

/** Does this dashboard belong to an account that may broadcast? */
export function isSubscribed(d: Dashboard): boolean {
  return d.subscribed ?? true;
}

export interface BroadcastEvent {
  id: number;
  broadcast_id: string;
  at: string;
  level: string;
  message: string;
}

/** A connected YouTube channel. Carries no token — the API has no such field. */
export interface YoutubeAccount {
  id: string;
  user_id: string;
  provider: string;
  channel_id: string;
  channel_title: string;
  thumbnail_url?: string | null;
  token_expiry?: string | null;
  created_at: string;
  updated_at: string;
  last_verified_at?: string | null;
}

/** Whether this deployment can offer YouTube connecting at all. */
export interface YoutubeAvailability {
  configured: boolean;
  /** The exact URI to register in the Google console. Not a secret. */
  redirect_uri: string;
}

/** YouTube's own view of a broadcast, which is not FFmpeg's. §8. */
export type YoutubeStatus =
  | "waiting_for_ingest"
  | "ready"
  | "live"
  | "complete"
  | "error";

export interface YoutubeLink {
  account_id?: string | null;
  broadcast_id?: string | null;
  stream_id?: string | null;
  status?: YoutubeStatus | string | null;
  watch_url?: string | null;
  last_error?: string | null;
}

export interface NewDestination {
  label: string;
  rtmps_url: string;
  /** Passed straight through to the server and never kept afterwards. */
  stream_key: string;
}

/** Korean labels for §2's states, so no component spells them itself. */
export const RUNTIME_LABELS: Record<RuntimeState, string> = {
  CREATED: "대기",
  PREPARING: "준비 중",
  STARTING: "시작 중",
  RUNNING: "송출 중",
  RECONNECTING: "재연결 중",
  STOPPING: "중지 중",
  STOPPED: "중지됨",
  FAILED: "실패",
};

export const MEDIA_LABELS: Record<MediaState, string> = {
  uploaded: "업로드됨",
  analysing: "분석 중",
  preparing: "변환 중",
  ready: "사용 가능",
  failed: "실패",
};

/** True when this broadcast is holding one of the plan's slots. */
export function holdsASlot(b: Broadcast): boolean {
  return ["PREPARING", "STARTING", "RUNNING", "RECONNECTING"].includes(
    b.runtime_state,
  );
}

/** What `/health` answers. No session needed: a monitor is usually asking. */
export interface Health {
  status: "ok" | "degraded";
  version: string;
  /** `local` — this computer. `cloud` — a server that stays on. */
  deployment: "local" | "cloud";
  checks: {
    api: boolean;
    database: boolean;
    ffmpeg: boolean;
    /** Whether this FFmpeg can publish to YouTube's RTMPS ingest at all. */
    ffmpeg_rtmps: boolean;
    storage: boolean;
  };
}

export interface BroadcastMetrics {
  id: string;
  name: string;
  runtime_state: RuntimeState;
  uptime_secs: number;
  bytes_sent: number;
  average_bitrate_bps: number;
  restart_count: number;
  last_error?: string | null;
  ffmpeg_pid?: number | null;
  last_heartbeat?: string | null;
}

export interface Metrics {
  deployment: "local" | "cloud";
  server: {
    cpu_percent: number;
    memory_total_bytes: number;
    memory_available_bytes: number;
    process_cpu_percent: number;
    process_memory_bytes: number;
    disk_available_bytes: number;
    egress_bytes: number;
  };
  broadcasts: BroadcastMetrics[];
}

/** What the header says about where this is running, and why it matters. */
export const DEPLOYMENT_LABELS: Record<
  Health["deployment"],
  { title: string; hint: string }
> = {
  local: {
    title: "LOCAL DEVELOPMENT",
    hint: "이 컴퓨터에서 서버가 실행 중입니다. 컴퓨터를 끄거나 절전되면 방송도 끝납니다.",
  },
  cloud: {
    title: "REMOTE CLOUD SERVER",
    hint: "서버에서 방송이 실행됩니다. 브라우저나 이 컴퓨터를 꺼도 방송은 계속됩니다.",
  },
};

export const PRIVACY_LABELS: Record<Privacy, string> = {
  public: "공개",
  unlisted: "일부 공개",
  private: "비공개",
};

/**
 * What YouTube itself last said. §8, §12.
 *
 * Deliberately separate from `RUNTIME_LABELS`: a running FFmpeg means bytes are
 * leaving this server, and it is not a promise that anything is on a channel.
 * The dashboard shows both, and neither is allowed to stand in for the other.
 */
export const YOUTUBE_STATUS_LABELS: Record<string, string> = {
  waiting_for_ingest: "연결 대기",
  ready: "신호 수신",
  live: "라이브",
  complete: "종료",
  error: "오류",
};

export function youtubeStatusLabel(b: Broadcast): string | null {
  const s = b.youtube?.status;
  if (!s) return null;
  return YOUTUBE_STATUS_LABELS[s] ?? s;
}

/** Which provider is sending this broadcast, for the card. §12. */
export function providerLabel(b: Broadcast): string {
  return b.youtube?.account_id ? "YouTube 계정" : "수동 RTMPS";
}

export const DAY_NAMES = ["월", "화", "수", "목", "금", "토", "일"] as const;
export const EVERY_DAY = 0b111_1111;
export const WEEKDAYS = 0b001_1111;

export function dayOn(mask: number, day: number): boolean {
  return (mask & (1 << day)) !== 0;
}

export function toggleDayMask(mask: number, day: number): number {
  return mask ^ (1 << day);
}

/** "2 / 8" and the rest of what a card shows about the playlist. */
export function playlistLabel(b: Broadcast): string {
  const total = b.play_count || b.item_count;
  if (total <= 0) return "—";
  const at = b.current_index > 0 ? b.current_index : 1;
  return `${at} / ${total}`;
}

/** How far into the current video, 0–100. */
export function itemPercent(b: Broadcast): number {
  if (b.current_duration_secs <= 0) return 0;
  return Math.min(
    100,
    (b.current_position_secs / b.current_duration_secs) * 100,
  );
}

/** The schedule in the reader's own timezone, or null when there is none. */
export function scheduleLabel(s: Schedule): string | null {
  if (!s.enabled || !s.start_at) return null;
  const start = new Date(s.start_at);
  const time = start.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  });
  if (s.repeat_days === EVERY_DAY) return `매일 ${time}`;
  if (s.repeat_days === WEEKDAYS) return `주중 ${time}`;
  if (s.repeat_days > 0) {
    const days = DAY_NAMES.filter((_, i) => dayOn(s.repeat_days, i)).join("·");
    return `${days} ${time}`;
  }
  return `${start.toLocaleDateString()} ${time}`;
}
