/**
 * The seven screens of the operator console.
 *
 * Each one answers a question an operator has at a particular moment, and the
 * order of the things on it is the order they ask: money first on the dashboard,
 * because that is what a business checks before anything else; then who is
 * paying, then what is on air, then whether the machine is coping.
 */
import { useState } from "react";
import {
  adminApi,
  bytes,
  dayAgo,
  localDate,
  localTime,
  won,
  type AdminBroadcastRow,
} from "./api";
import { Bars, Cell, Change, Confirm, Empty, Failed, Kpi, Loading, Panel, Row, Table, Tag, useLoad } from "./ui";

const PLAN_TONE: Record<string, string> = {
  basic: "default",
  pro: "ok",
  business: "live",
  none: "default",
};

function planTag(planId: string, label?: string) {
  return <Tag tone={PLAN_TONE[planId] ?? "default"}>{label ?? planId}</Tag>;
}

function stateTag(b: { desired_state: string; runtime_state: string; worker_alive: boolean }) {
  if (b.runtime_state === "FAILED") return <Tag tone="live">오류</Tag>;
  if (b.desired_state === "running" && !b.worker_alive) return <Tag tone="warn">워커 없음</Tag>;
  if (b.desired_state === "running") return <Tag tone="ok">송출 중</Tag>;
  return <Tag>중지</Tag>;
}

// --- dashboard -------------------------------------------------------------

export function AdminDashboardPage({ go }: { go: (path: string) => void }) {
  const { data, error, busy, reload } = useLoad(() => adminApi.dashboard(), []);
  if (busy && !data) return <Loading />;
  if (error) return <Failed error={error} onRetry={reload} />;
  if (!data) return null;

  const d = data;
  const diskLow =
    d.disk_free_bytes !== null && d.disk_free_bytes < d.disk_floor_bytes;
  const trouble = d.broadcasts.failed > 0 || d.subscriptions.failed_this_month > 0 || diskLow;

  return (
    <div className="space-y-6">
      {trouble && (
        <div role="alert" className="rounded-md border border-warn-dim bg-ink-900 px-4 py-3 text-sm text-warn">
          {d.broadcasts.failed > 0 && <div>· 오류 상태인 방송 {d.broadcasts.failed}개</div>}
          {d.subscriptions.failed_this_month > 0 && (
            <div>· 이번 달 결제 실패 {d.subscriptions.failed_this_month}건</div>
          )}
          {diskLow && <div>· 서버 저장 공간이 부족합니다 (업로드가 거부됩니다)</div>}
        </div>
      )}

      <section>
        <h2 className="mb-2 text-xs font-semibold uppercase tracking-widest text-ink-500">매출</h2>
        <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
          <Kpi label="오늘 매출" value={won(d.revenue.today)} tone="ok" />
          <Kpi
            label="이번 달 매출"
            value={won(d.revenue.this_month)}
            sub={<Change pct={d.revenue.month_change_pct} />}
          />
          <Kpi label="MRR" value={won(d.mrr_krw)} sub="현재 정기결제 합계" />
          <Kpi label="누적 실결제" value={won(d.revenue.all_time)} />
        </div>
      </section>

      <section>
        <h2 className="mb-2 text-xs font-semibold uppercase tracking-widest text-ink-500">회원</h2>
        <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
          <Kpi label="전체 회원" value={d.users.total.toLocaleString("ko-KR")} sub={`비활성 ${d.users.disabled}`} />
          <Kpi label="오늘 가입" value={d.users.today} sub={`7일 ${d.users.last_7_days} · 30일 ${d.users.last_30_days}`} />
          <Kpi label="유료 회원" value={d.subscriptions.paid_total} sub={`미구독 ${d.subscriptions.unsubscribed}`} />
          <Kpi
            label="이번 달"
            value={`신규 ${d.subscriptions.new_this_month} · 갱신 ${d.subscriptions.renewed_this_month}`}
            sub={`해지 ${d.subscriptions.cancelled_this_month} · 실패 ${d.subscriptions.failed_this_month}`}
            tone={d.subscriptions.failed_this_month > 0 ? "warn" : "default"}
          />
        </div>
        <div className="mt-3 grid grid-cols-3 gap-3">
          {d.subscriptions.by_plan.map((p) => (
            <Kpi key={p.plan_id} label={p.label} value={p.count} sub={won(p.monthly_price_krw)} />
          ))}
        </div>
      </section>

      <section>
        <h2 className="mb-2 text-xs font-semibold uppercase tracking-widest text-ink-500">서비스</h2>
        <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
          <Kpi label="송출 중" value={d.broadcasts.running} tone={d.broadcasts.running > 0 ? "ok" : "default"} />
          <Kpi label="예약 방송" value={d.broadcasts.scheduled} />
          <Kpi label="오류 방송" value={d.broadcasts.failed} tone={d.broadcasts.failed > 0 ? "live" : "default"} />
          <Kpi label="YouTube 연결" value={d.youtube_accounts} />
        </div>
        <div className="mt-3 grid grid-cols-2 gap-3">
          <Kpi label="미디어 사용량" value={bytes(d.storage_bytes)} />
          <Kpi
            label="서버 여유 공간"
            value={d.disk_free_bytes === null ? "알 수 없음" : bytes(d.disk_free_bytes)}
            sub={`업로드 차단 기준 ${bytes(d.disk_floor_bytes)}`}
            tone={diskLow ? "live" : "default"}
          />
        </div>
      </section>

      <div className="flex flex-wrap gap-2">
        <button onClick={() => go("/admin/revenue")} className="rounded-md border border-ink-600 px-3 py-1.5 text-xs text-ink-300 hover:bg-ink-800">
          매출 자세히
        </button>
        <button onClick={() => go("/admin/users")} className="rounded-md border border-ink-600 px-3 py-1.5 text-xs text-ink-300 hover:bg-ink-800">
          회원 관리
        </button>
        <button onClick={() => go("/admin/broadcasts")} className="rounded-md border border-ink-600 px-3 py-1.5 text-xs text-ink-300 hover:bg-ink-800">
          방송 관리
        </button>
      </div>
    </div>
  );
}

// --- revenue ---------------------------------------------------------------

const RANGES: { id: string; label: string; from: () => string; grain: string }[] = [
  { id: "7", label: "7일", from: () => dayAgo(6), grain: "day" },
  { id: "30", label: "30일", from: () => dayAgo(29), grain: "day" },
  { id: "90", label: "90일", from: () => dayAgo(89), grain: "week" },
  { id: "month", label: "이번 달", from: () => `${dayAgo(0).slice(0, 7)}-01`, grain: "day" },
  { id: "year", label: "올해", from: () => `${dayAgo(0).slice(0, 4)}-01-01`, grain: "month" },
];

export function AdminRevenuePage() {
  const [range, setRange] = useState("30");
  const [custom, setCustom] = useState<{ from: string; to: string } | null>(null);
  const chosen = RANGES.find((r) => r.id === range) ?? RANGES[1]!;
  const from = custom?.from ?? chosen.from();
  const to = custom?.to ?? dayAgo(0);
  const grain = custom ? "day" : chosen.grain;
  const { data, error, busy, reload } = useLoad(
    () => adminApi.revenue(from, to, grain),
    [from, to, grain],
  );

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-end gap-2">
        {RANGES.map((r) => (
          <button
            key={r.id}
            onClick={() => {
              setCustom(null);
              setRange(r.id);
            }}
            className={`rounded-md px-2.5 py-1 text-xs ${
              !custom && range === r.id
                ? "bg-ink-800 text-ink-100"
                : "border border-ink-700 text-ink-400 hover:text-ink-100"
            }`}
          >
            {r.label}
          </button>
        ))}
        <label className="ml-2 flex items-center gap-1 text-[11px] text-ink-500">
          <input
            type="date"
            value={from}
            onChange={(e) => setCustom({ from: e.target.value, to })}
            className="rounded border border-ink-700 bg-ink-950 px-2 py-1 text-xs text-ink-200"
            aria-label="시작 날짜"
          />
          –
          <input
            type="date"
            value={to}
            onChange={(e) => setCustom({ from, to: e.target.value })}
            className="rounded border border-ink-700 bg-ink-950 px-2 py-1 text-xs text-ink-200"
            aria-label="종료 날짜"
          />
        </label>
      </div>

      {busy && !data ? (
        <Loading />
      ) : error ? (
        <Failed error={error} onRetry={reload} />
      ) : !data ? null : (
        <>
          <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
            <Kpi label="이번 달 매출" value={won(data.summary.this_month)} sub={<Change pct={data.summary.month_change_pct} />} tone="ok" />
            <Kpi label="지난 달 매출" value={won(data.summary.last_month)} />
            <Kpi label="MRR" value={won(data.mrr_krw)} sub={`유료 ${data.active_payers}명`} />
            <Kpi label="ARPU" value={data.arpu_krw === null ? "—" : won(data.arpu_krw)} sub="이번 달 매출 ÷ 유료 회원" />
          </div>
          <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
            <Kpi label="오늘" value={won(data.summary.today)} />
            <Kpi label="어제" value={won(data.summary.yesterday)} />
            <Kpi label="이번 주" value={won(data.summary.this_week)} />
            <Kpi label="올해" value={won(data.summary.this_year)} />
          </div>

          <Panel
            title={`기간 매출 · ${data.from} ~ ${data.to}`}
            action={
              <span className="font-mono text-xs text-ink-300">
                {won(data.summary.all_time)}
                <span className="ml-2 text-ink-600">실결제 기준</span>
              </span>
            }
          >
            <Bars
              data={data.buckets.map((b) => ({
                label: b.period,
                total: b.krw,
                note: `결제 ${b.payments}건 · 신규 ${won(b.new_krw)} · 갱신 ${won(b.renewal_krw)}`,
                parts: [
                  { value: b.new_krw, tone: "#22c55e", name: "신규" },
                  { value: b.renewal_krw, tone: "#3b82f6", name: "갱신" },
                ],
              }))}
            />
            <div className="mt-3 flex gap-4 text-[11px] text-ink-500">
              <span><span className="mr-1 inline-block h-2 w-2 rounded-sm" style={{ background: "#22c55e" }} />신규 결제</span>
              <span><span className="mr-1 inline-block h-2 w-2 rounded-sm" style={{ background: "#3b82f6" }} />정기 갱신</span>
              {data.reversals > 0 && <span className="text-warn">· 이 기간 취소·환불 알림 {data.reversals}건 (매출에서 차감하지 않음)</span>}
            </div>
          </Panel>

          <div className="grid gap-4 md:grid-cols-2">
            <Panel title="플랜별 매출">
              {data.by_plan.length === 0 ? (
                <Empty title="이 기간에는 결제가 없습니다." />
              ) : (
                <Table head={["플랜", "매출", "건수"]}>
                  {data.by_plan.map((p) => (
                    <Row key={p.plan_id}>
                      <Cell>{planTag(p.plan_id, p.label)}</Cell>
                      <Cell mono>{won(p.krw)}</Cell>
                      <Cell mono>{p.payments}</Cell>
                    </Row>
                  ))}
                </Table>
              )}
            </Panel>
            <Panel title="최근 결제">
              {data.recent.length === 0 ? (
                <Empty title="아직 결제 내역이 없습니다." />
              ) : (
                <Table head={["일시", "회원", "금액", "구분"]}>
                  {data.recent.map((p, i) => (
                    <Row key={`${p.at}-${i}`}>
                      <Cell mono>{localTime(p.at)}</Cell>
                      <Cell className="max-w-[12rem] truncate">{p.email ?? "—"}</Cell>
                      <Cell mono>{won(p.amount_krw)}</Cell>
                      <Cell>
                        {p.kind === "paid" ? (
                          <Tag tone="ok">{p.is_first ? "신규" : "갱신"}</Tag>
                        ) : (
                          <Tag tone="warn">{p.outcome.split(" ")[0]}</Tag>
                        )}
                      </Cell>
                    </Row>
                  ))}
                </Table>
              )}
            </Panel>
          </div>
        </>
      )}
    </div>
  );
}

// --- users -----------------------------------------------------------------

const FILTERS = [
  { id: "all", label: "전체" },
  { id: "paid", label: "유료" },
  { id: "basic", label: "Basic" },
  { id: "pro", label: "Pro" },
  { id: "business", label: "Business" },
  { id: "none", label: "미구독" },
  { id: "youtube", label: "YouTube 연결" },
  { id: "live", label: "방송 중" },
  { id: "disabled", label: "비활성" },
];

const SORTS = [
  { id: "created", label: "가입일" },
  { id: "active", label: "최근 활동" },
  { id: "plan", label: "플랜" },
  { id: "storage", label: "저장 사용량" },
  { id: "email", label: "이메일" },
];

const PAGE = 50;

export function AdminUsersPage({ go }: { go: (path: string) => void }) {
  const [q, setQ] = useState("");
  const [typed, setTyped] = useState("");
  const [filter, setFilter] = useState("all");
  const [sort, setSort] = useState("created");
  const [offset, setOffset] = useState(0);
  const { data, error, busy, reload } = useLoad(
    () => adminApi.users({ q, filter, sort, limit: PAGE, offset }),
    [q, filter, sort, offset],
  );

  return (
    <div className="space-y-4">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          setOffset(0);
          setQ(typed);
        }}
        className="flex flex-wrap items-center gap-2"
      >
        <input
          value={typed}
          onChange={(e) => setTyped(e.target.value)}
          placeholder="이름 또는 이메일"
          aria-label="회원 검색"
          className="w-56 rounded-md border border-ink-700 bg-ink-950 px-3 py-1.5 text-sm text-ink-100"
        />
        <button className="rounded-md border border-ink-600 px-3 py-1.5 text-sm text-ink-300 hover:bg-ink-800">
          검색
        </button>
        <select
          value={sort}
          onChange={(e) => setSort(e.target.value)}
          aria-label="정렬"
          className="rounded-md border border-ink-700 bg-ink-950 px-2 py-1.5 text-sm text-ink-200"
        >
          {SORTS.map((s) => (
            <option key={s.id} value={s.id}>
              {s.label}순
            </option>
          ))}
        </select>
      </form>

      <div className="flex flex-wrap gap-1.5">
        {FILTERS.map((f) => (
          <button
            key={f.id}
            onClick={() => {
              setOffset(0);
              setFilter(f.id);
            }}
            className={`rounded-md px-2.5 py-1 text-xs ${
              filter === f.id ? "bg-ink-800 text-ink-100" : "border border-ink-700 text-ink-400 hover:text-ink-100"
            }`}
          >
            {f.label}
          </button>
        ))}
      </div>

      {busy && !data ? (
        <Loading />
      ) : error ? (
        <Failed error={error} onRetry={reload} />
      ) : !data || data.length === 0 ? (
        <Empty title="조건에 맞는 회원이 없습니다." hint="검색어나 필터를 바꿔 보세요." />
      ) : (
        <>
          <Table head={["회원", "가입", "플랜", "구독", "저장", "YouTube", "방송", "누적 결제"]}>
            {data.map((u) => (
              <Row key={u.id} onClick={() => go(`/admin/users/${u.id}`)}>
                <Cell>
                  <div className="flex items-center gap-1.5">
                    <span className="text-ink-100">{u.name ?? u.email}</span>
                    {u.role === "admin" && <Tag tone="live">관리자</Tag>}
                    {u.disabled_at && <Tag tone="warn">비활성</Tag>}
                  </div>
                  <div className="text-[11px] text-ink-500">{u.email}</div>
                </Cell>
                <Cell mono>{localDate(u.created_at)}</Cell>
                <Cell>{planTag(u.plan_id, u.plan_label)}</Cell>
                <Cell>
                  <div className="text-xs text-ink-300">{u.billing_status ?? u.subscription_status}</div>
                  {u.next_charge_at && <div className="text-[11px] text-ink-600">~{u.next_charge_at}</div>}
                </Cell>
                <Cell mono>
                  {bytes(u.storage_bytes)}
                  <span className="text-ink-600"> / {bytes(u.storage_limit_bytes)}</span>
                </Cell>
                <Cell mono>{u.youtube_accounts || "—"}</Cell>
                <Cell mono>{u.running_broadcasts > 0 ? <Tag tone="ok">{u.running_broadcasts}</Tag> : "—"}</Cell>
                <Cell mono>{u.total_paid_krw > 0 ? won(u.total_paid_krw) : "—"}</Cell>
              </Row>
            ))}
          </Table>
          <div className="flex items-center justify-between text-xs text-ink-500">
            <span>{offset + 1}–{offset + data.length}</span>
            <span className="flex gap-2">
              <button
                disabled={offset === 0}
                onClick={() => setOffset(Math.max(0, offset - PAGE))}
                className="rounded border border-ink-700 px-2 py-1 disabled:opacity-40"
              >
                이전
              </button>
              <button
                disabled={data.length < PAGE}
                onClick={() => setOffset(offset + PAGE)}
                className="rounded border border-ink-700 px-2 py-1 disabled:opacity-40"
              >
                다음
              </button>
            </span>
          </div>
        </>
      )}
    </div>
  );
}

// --- one user --------------------------------------------------------------

export function AdminUserPage({ id, go }: { id: string; go: (path: string) => void }) {
  const { data, error, busy, reload } = useLoad(() => adminApi.user(id), [id]);
  const [asking, setAsking] = useState<null | "disable" | "enable">(null);
  const [note, setNote] = useState("");
  const [working, setWorking] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  if (busy && !data) return <Loading />;
  if (error) return <Failed error={error} onRetry={reload} />;
  if (!data) return null;
  const u = data.user;

  const act = async () => {
    setWorking(true);
    setFailed(null);
    try {
      await adminApi.setDisabled(u.id, asking === "disable", note || undefined);
      setAsking(null);
      setNote("");
      reload();
    } catch (e) {
      setFailed(e instanceof Error ? e.message : "처리하지 못했습니다.");
    } finally {
      setWorking(false);
    }
  };

  return (
    <div className="space-y-5">
      <button onClick={() => go("/admin/users")} className="text-xs text-ink-500 hover:text-ink-200">
        ← 회원 목록
      </button>

      {failed && (
        <p role="alert" className="rounded-md border border-live-dim bg-ink-900 px-3 py-2 text-sm text-live">
          {failed}
        </p>
      )}

      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="flex items-center gap-2 text-lg text-ink-100">
            {u.name ?? u.email}
            {u.role === "admin" && <Tag tone="live">관리자</Tag>}
            {u.disabled_at && <Tag tone="warn">비활성 · {localDate(u.disabled_at)}</Tag>}
          </h1>
          <p className="font-mono text-xs text-ink-500">{u.email}</p>
        </div>
        <button
          onClick={() => setAsking(u.disabled_at ? "enable" : "disable")}
          className={`rounded-md px-3 py-1.5 text-sm ${
            u.disabled_at
              ? "border border-ink-600 text-ink-200 hover:bg-ink-800"
              : "bg-live text-white hover:brightness-110"
          }`}
        >
          {u.disabled_at ? "계정 다시 활성화" : "계정 비활성화"}
        </button>
      </div>

      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        <Kpi label="가입일" value={localDate(u.created_at)} />
        <Kpi label="현재 플랜" value={u.plan_label} sub={u.billing_status ?? u.subscription_status} />
        <Kpi label="누적 실결제" value={won(u.total_paid_krw)} sub={`${data.payment_count}건`} />
        <Kpi
          label="저장 사용량"
          value={bytes(u.storage_bytes)}
          sub={`한도 ${bytes(u.storage_limit_bytes)} · 영상 ${data.media_count}개`}
          tone={u.storage_limit_bytes > 0 && u.storage_bytes > u.storage_limit_bytes ? "warn" : "default"}
        />
      </div>

      <div className="grid gap-4 md:grid-cols-2">
        <Panel title="구독">
          {data.billing.length === 0 ? (
            <Empty title="결제 기록이 없습니다." hint="이 계정은 결제한 적이 없습니다." />
          ) : (
            <dl className="space-y-1.5 text-sm">
              {data.billing.map((b) => (
                <div key={b.id} className="rounded border border-ink-800 p-2">
                  <div className="flex items-center justify-between">
                    {planTag(b.plan_id)}
                    <Tag tone={b.status === "active" ? "ok" : b.status === "cancelled" ? "warn" : "default"}>
                      {b.status}
                    </Tag>
                  </div>
                  <div className="mt-1 grid grid-cols-2 gap-x-3 text-[11px] text-ink-500">
                    <span>월 {won(b.amount_krw)}</span>
                    <span>시작 {localDate(b.activated_at)}</span>
                    <span>최근 결제 {localDate(b.last_paid_at)}</span>
                    <span>다음 {b.current_period_end ?? "—"}</span>
                    {b.cancelled_at && <span className="text-warn">해지 {localDate(b.cancelled_at)}</span>}
                  </div>
                </div>
              ))}
            </dl>
          )}
        </Panel>

        <Panel title="YouTube">
          {data.youtube_channels.length === 0 ? (
            <Empty title="연결된 채널이 없습니다." />
          ) : (
            <ul className="space-y-1 text-sm text-ink-200">
              {data.youtube_channels.map((c) => (
                <li key={c}>· {c}</li>
              ))}
            </ul>
          )}
          <p className="mt-3 text-[11px] text-ink-600">
            토큰과 스트림 키는 관리자에게도 표시되지 않습니다.
          </p>
        </Panel>
      </div>

      <Panel title={`방송 ${data.broadcasts.length}개`}>
        {data.broadcasts.length === 0 ? (
          <Empty title="만든 방송이 없습니다." />
        ) : (
          <Table head={["이름", "상태", "시작", "재시작", "항목", "최근 오류"]}>
            {data.broadcasts.map((b) => (
              <Row key={b.id}>
                <Cell>
                  {b.name}
                  <div className="text-[11px] text-ink-600">{b.loop_forever ? "전체 반복" : "한 바퀴"}</div>
                </Cell>
                <Cell>{stateTag(b)}</Cell>
                <Cell mono>{localTime(b.started_at)}</Cell>
                <Cell mono>{b.restart_count}</Cell>
                <Cell mono>{b.item_count}</Cell>
                <Cell className="max-w-[16rem] truncate text-[11px] text-ink-500">{b.last_error ?? "—"}</Cell>
              </Row>
            ))}
          </Table>
        )}
      </Panel>

      <Panel title="결제 이력">
        {data.payments.length === 0 ? (
          <Empty title="아직 결제 내역이 없습니다." />
        ) : (
          <Table head={["일시", "금액", "플랜", "구분", "상태", "참조"]}>
            {data.payments.map((p, i) => (
              <Row key={`${p.at}-${i}`}>
                <Cell mono>{localTime(p.at)}</Cell>
                <Cell mono>{won(p.amount_krw)}</Cell>
                <Cell>{p.plan_id ? planTag(p.plan_id) : "—"}</Cell>
                <Cell>
                  {p.kind === "paid" ? <Tag tone="ok">{p.is_first ? "신규" : "갱신"}</Tag> : <Tag tone="warn">{p.kind}</Tag>}
                </Cell>
                <Cell className="max-w-[14rem] truncate text-[11px] text-ink-500">{p.outcome}</Cell>
                <Cell mono className="text-ink-600">{p.provider_ref ?? "—"}</Cell>
              </Row>
            ))}
          </Table>
        )}
      </Panel>

      {asking && (
        <Confirm
          title={asking === "disable" ? "계정을 비활성화하시겠습니까?" : "계정을 다시 활성화하시겠습니까?"}
          detail={
            asking === "disable"
              ? "로그인과 새 방송 시작이 차단되고, 지금 송출 중인 방송은 즉시 종료됩니다. 영상·결제 기록은 삭제되지 않습니다. PayApp 정기결제는 그대로 남으므로, 결제를 멈추려면 해지를 따로 처리해야 합니다."
              : "이 계정이 다시 로그인하고 방송할 수 있게 됩니다."
          }
          confirmLabel={asking === "disable" ? "비활성화" : "활성화"}
          busy={working}
          note={note}
          onNote={setNote}
          onConfirm={act}
          onCancel={() => setAsking(null)}
        />
      )}
    </div>
  );
}

// --- broadcasts ------------------------------------------------------------

const B_FILTERS = [
  { id: "all", label: "전체" },
  { id: "running", label: "송출 중" },
  { id: "scheduled", label: "예약" },
  { id: "stopped", label: "중지" },
  { id: "failed", label: "오류" },
];

export function AdminBroadcastsPage({ go }: { go: (path: string) => void }) {
  const [filter, setFilter] = useState("all");
  const { data, error, busy, reload } = useLoad(
    () => adminApi.broadcasts({ filter, limit: 100 }),
    [filter],
  );
  const [stopping, setStopping] = useState<AdminBroadcastRow | null>(null);
  const [note, setNote] = useState("");
  const [working, setWorking] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const stop = async () => {
    if (!stopping) return;
    setWorking(true);
    setFailed(null);
    try {
      await adminApi.forceStop(stopping.id, note || undefined);
      setStopping(null);
      setNote("");
      reload();
    } catch (e) {
      setFailed(e instanceof Error ? e.message : "중지하지 못했습니다.");
    } finally {
      setWorking(false);
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap gap-1.5">
        {B_FILTERS.map((f) => (
          <button
            key={f.id}
            onClick={() => setFilter(f.id)}
            className={`rounded-md px-2.5 py-1 text-xs ${
              filter === f.id ? "bg-ink-800 text-ink-100" : "border border-ink-700 text-ink-400 hover:text-ink-100"
            }`}
          >
            {f.label}
          </button>
        ))}
      </div>

      {failed && (
        <p role="alert" className="rounded-md border border-live-dim bg-ink-900 px-3 py-2 text-sm text-live">
          {failed}
        </p>
      )}

      {busy && !data ? (
        <Loading />
      ) : error ? (
        <Failed error={error} onRetry={reload} />
      ) : !data || data.length === 0 ? (
        <Empty title="조건에 맞는 방송이 없습니다." />
      ) : (
        <Table head={["방송", "회원", "상태", "YouTube", "시작", "재시작", "최근 오류", ""]}>
          {data.map((b) => (
            <Row key={b.id}>
              <Cell>
                <div className="text-ink-100">{b.name}</div>
                <div className="text-[11px] text-ink-600">
                  {b.item_count}개 · {b.loop_forever ? "전체 반복" : "한 바퀴"}
                  {b.scheduled && " · 예약"}
                </div>
              </Cell>
              <Cell>
                <button onClick={() => go(`/admin/users/${b.user_id}`)} className="text-ink-300 hover:text-ink-100">
                  {b.email}
                </button>
              </Cell>
              <Cell>
                {stateTag(b)}
                <div className="mt-0.5 font-mono text-[10px] text-ink-600">{b.runtime_state}</div>
              </Cell>
              <Cell className="text-xs">
                {b.youtube_channel ? (
                  <>
                    <div className="text-ink-300">{b.youtube_channel}</div>
                    <div className="text-[10px] text-ink-600">{b.youtube_status ?? "—"}</div>
                  </>
                ) : (
                  <span className="text-ink-600">직접 입력</span>
                )}
              </Cell>
              <Cell mono>{localTime(b.started_at)}</Cell>
              <Cell mono>{b.restart_count}</Cell>
              <Cell className="max-w-[14rem] truncate text-[11px] text-ink-500">{b.last_error ?? "—"}</Cell>
              <Cell>
                {b.desired_state === "running" && (
                  <button
                    onClick={() => setStopping(b)}
                    className="rounded border border-live-dim px-2 py-1 text-[11px] text-live hover:bg-ink-800"
                  >
                    강제 종료
                  </button>
                )}
              </Cell>
            </Row>
          ))}
        </Table>
      )}

      {stopping && (
        <Confirm
          title="이 방송을 강제 종료하시겠습니까?"
          detail={`${stopping.email} 님의 「${stopping.name}」 송출이 즉시 중지됩니다. 사용자가 중지 버튼을 누른 것과 동일하게 처리되며, YouTube 방송도 정상 종료됩니다.`}
          confirmLabel="강제 종료"
          busy={working}
          note={note}
          onNote={setNote}
          onConfirm={stop}
          onCancel={() => setStopping(null)}
        />
      )}
    </div>
  );
}

// --- billing ---------------------------------------------------------------

export function AdminBillingPage({ go }: { go: (path: string) => void }) {
  const [kind, setKind] = useState<string | undefined>(undefined);
  const { data, error, busy, reload } = useLoad(() => adminApi.billing({ kind, limit: 100 }), [kind]);

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap gap-1.5">
        {[
          { id: undefined, label: "전체" },
          { id: "paid", label: "성공" },
          { id: "failed", label: "실패·취소" },
        ].map((f) => (
          <button
            key={f.label}
            onClick={() => setKind(f.id)}
            className={`rounded-md px-2.5 py-1 text-xs ${
              kind === f.id ? "bg-ink-800 text-ink-100" : "border border-ink-700 text-ink-400 hover:text-ink-100"
            }`}
          >
            {f.label}
          </button>
        ))}
      </div>

      {busy && !data ? (
        <Loading />
      ) : error ? (
        <Failed error={error} onRetry={reload} />
      ) : !data ? null : (
        <>
          {!data.configured && (
            <p className="rounded-md border border-warn-dim bg-ink-900 px-3 py-2 text-sm text-warn">
              이 서버에는 PayApp 이 설정되어 있지 않습니다. 기록은 보이지만 새 결제는 받을 수 없습니다.
            </p>
          )}

          {data.mismatches.length > 0 && (
            <Panel title="주의 필요">
              <p className="mb-3 text-xs text-warn">
                정기결제는 해지됐는데 유료 권한이 남아 있는 계정입니다. 서버에서{" "}
                <code className="font-mono">louver-server --audit-billing --fix --yes</code> 로 정리할 수 있습니다.
              </p>
              <Table head={["회원", "플랜", "결제 상태", "해지"]}>
                {data.mismatches.map((m) => (
                  <Row key={m.billing_id} onClick={() => go(`/admin/users/${m.user_id}`)}>
                    <Cell>{m.email}</Cell>
                    <Cell>{planTag(m.plan_id)}</Cell>
                    <Cell>{m.billing_status}</Cell>
                    <Cell mono>{localTime(m.cancelled_at)}</Cell>
                  </Row>
                ))}
              </Table>
            </Panel>
          )}

          <Panel title="결제 기록">
            {data.payments.length === 0 ? (
              <Empty title="아직 결제 내역이 없습니다." />
            ) : (
              <Table head={["일시", "회원", "금액", "플랜", "구분", "결과"]}>
                {data.payments.map((p, i) => (
                  <Row key={`${p.at}-${i}`} onClick={p.user_id ? () => go(`/admin/users/${p.user_id}`) : undefined}>
                    <Cell mono>{localTime(p.at)}</Cell>
                    <Cell className="max-w-[14rem] truncate">{p.email ?? "—"}</Cell>
                    <Cell mono>{won(p.amount_krw)}</Cell>
                    <Cell>{p.plan_id ? planTag(p.plan_id) : "—"}</Cell>
                    <Cell>
                      {p.kind === "paid" ? (
                        <Tag tone="ok">{p.is_first ? "신규" : "갱신"}</Tag>
                      ) : p.kind === "reversal" ? (
                        <Tag tone="live">취소</Tag>
                      ) : (
                        <Tag tone="warn">{p.kind}</Tag>
                      )}
                    </Cell>
                    <Cell className="max-w-[16rem] truncate text-[11px] text-ink-500">{p.outcome}</Cell>
                  </Row>
                ))}
              </Table>
            )}
          </Panel>

          <Panel title={`해지 ${data.cancellations.length}건`}>
            {data.cancellations.length === 0 ? (
              <Empty title="해지한 회원이 없습니다." />
            ) : (
              <Table head={["회원", "기존 플랜", "해지", "최근 결제", "현재 권한", "방송"]}>
                {data.cancellations.map((c) => (
                  <Row key={`${c.user_id}-${c.cancelled_at}`} onClick={() => go(`/admin/users/${c.user_id}`)}>
                    <Cell>{c.email}</Cell>
                    <Cell>{planTag(c.plan_id)}</Cell>
                    <Cell mono>{localTime(c.cancelled_at)}</Cell>
                    <Cell mono>{localDate(c.last_paid_at)}</Cell>
                    <Cell>
                      {c.entitlement_active ? (
                        <Tag tone="warn">{c.entitlement_plan} 유지</Tag>
                      ) : (
                        <Tag>미구독</Tag>
                      )}
                    </Cell>
                    <Cell mono>{c.running_broadcasts}</Cell>
                  </Row>
                ))}
              </Table>
            )}
          </Panel>
        </>
      )}
    </div>
  );
}

// --- system ----------------------------------------------------------------

export function AdminSystemPage() {
  const [tick, setTick] = useState(0);
  const { data, error, busy, reload } = useLoad(() => adminApi.system(), [tick]);

  // Twenty seconds: often enough to watch an incident, rare enough that the
  // page costs the server almost nothing while it is open.
  useState(() => {
    const t = setInterval(() => setTick((n) => n + 1), 20_000);
    return () => clearInterval(t);
  });

  if (busy && !data) return <Loading />;
  if (error) return <Failed error={error} onRetry={reload} />;
  if (!data) return null;

  const memPct = data.memory_total_mb > 0 ? (data.memory_used_mb / data.memory_total_mb) * 100 : 0;
  const diskLow = data.disk_free_bytes > 0 && data.disk_free_bytes < data.disk_floor_bytes;

  return (
    <div className="space-y-5">
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        <Kpi
          label="서비스"
          value={data.health.status === "ok" ? "정상" : "이상"}
          tone={data.health.status === "ok" ? "ok" : "live"}
          sub={`v${data.health.version} · ${data.health.deployment}`}
        />
        <Kpi label="CPU" value={`${data.cpu_percent.toFixed(0)}%`} tone={data.cpu_percent > 85 ? "warn" : "default"} />
        <Kpi
          label="메모리"
          value={`${memPct.toFixed(0)}%`}
          sub={`${data.memory_used_mb} / ${data.memory_total_mb} MB`}
          tone={memPct > 90 ? "warn" : "default"}
        />
        <Kpi
          label="디스크 여유"
          value={bytes(data.disk_free_bytes)}
          sub={`차단 기준 ${bytes(data.disk_floor_bytes)}`}
          tone={diskLow ? "live" : "default"}
        />
      </div>

      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        <Kpi label="FFmpeg 워커" value={data.workers} />
        <Kpi label="송출 중" value={data.running_broadcasts} />
        <Kpi label="미디어 사용량" value={bytes(data.storage_bytes)} />
        <Kpi label="쿠키 정책" value={data.health.cookies} />
      </div>

      <Panel title="상태 점검">
        <div className="grid grid-cols-2 gap-2 md:grid-cols-3">
          {Object.entries(data.health.checks).map(([name, ok]) => (
            <div key={name} className="flex items-center justify-between rounded border border-ink-800 px-3 py-2">
              <span className="font-mono text-xs text-ink-400">{name}</span>
              <Tag tone={ok ? "ok" : "live"}>{ok ? "OK" : "실패"}</Tag>
            </div>
          ))}
        </div>
      </Panel>

      <Panel title="저장 공간 상위 회원">
        {data.storage_leaders.length === 0 ? (
          <Empty title="업로드된 영상이 없습니다." />
        ) : (
          <Table head={["회원", "플랜", "사용량", "한도", "파일"]}>
            {data.storage_leaders.map((s) => (
              <Row key={s.user_id}>
                <Cell>{s.email}</Cell>
                <Cell>{planTag(s.plan_id)}</Cell>
                <Cell mono>{bytes(s.bytes)}</Cell>
                <Cell mono className={s.limit_bytes > 0 && s.bytes > s.limit_bytes ? "text-warn" : ""}>
                  {bytes(s.limit_bytes)}
                </Cell>
                <Cell mono>{s.files}</Cell>
              </Row>
            ))}
          </Table>
        )}
      </Panel>

      <Panel title="최근 문제">
        {data.problems.length === 0 ? (
          <Empty title="최근 오류가 없습니다." />
        ) : (
          <Table head={["시각", "방송", "회원", "내용"]}>
            {data.problems.map((p, i) => (
              <Row key={`${p.at}-${i}`}>
                <Cell mono>{localTime(p.at)}</Cell>
                <Cell>{p.broadcast_name}</Cell>
                <Cell className="max-w-[12rem] truncate text-[11px] text-ink-500">{p.email}</Cell>
                <Cell className="text-[11px]">
                  <Tag tone={p.level === "error" ? "live" : "warn"}>{p.level}</Tag>{" "}
                  <span className="text-ink-400">{p.message}</span>
                </Cell>
              </Row>
            ))}
          </Table>
        )}
      </Panel>
    </div>
  );
}

// --- audit -----------------------------------------------------------------

export function AdminAuditPage() {
  const { data, error, busy, reload } = useLoad(() => adminApi.audit({ limit: 100 }), []);
  if (busy && !data) return <Loading />;
  if (error) return <Failed error={error} onRetry={reload} />;
  if (!data || data.length === 0)
    return <Empty title="아직 관리자 작업 기록이 없습니다." hint="운영자가 무언가를 바꾸면 여기에 남습니다." />;

  return (
    <Table head={["시각", "운영자", "작업", "대상", "변경", "사유"]}>
      {data.map((a) => (
        <Row key={a.id}>
          <Cell mono>{localTime(a.at)}</Cell>
          <Cell className="max-w-[12rem] truncate">{a.admin_email}</Cell>
          <Cell mono className="text-ink-200">{a.action}</Cell>
          <Cell mono className="max-w-[10rem] truncate text-ink-500">
            {a.target_type}/{a.target_id.slice(0, 8)}
          </Cell>
          <Cell className="text-[11px] text-ink-500">
            {a.before || a.after ? `${a.before ?? "—"} → ${a.after ?? "—"}` : "—"}
          </Cell>
          <Cell className="max-w-[14rem] truncate text-[11px] text-ink-400">{a.note ?? "—"}</Cell>
        </Row>
      ))}
    </Table>
  );
}
