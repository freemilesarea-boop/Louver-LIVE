/**
 * The operator's side of a manual grant: the form, and the panel on a member.
 *
 * One rule runs through all of it. A grant is not a payment and must never
 * read like one, so every screen here says where an entitlement came from
 * rather than only what it is — "Business · 관리자 지급 · D-29" rather than
 * "Business". An operator looking at an account has to be able to tell in one
 * glance whether money is involved, because what they may do about it differs
 * entirely.
 */
import { useState } from "react";
import { adminApi, localDate, type AdminUserRow, type Grant, type GrantOutcome } from "./api";
import { Cell, Panel, Row, Table, Tag } from "./ui";

const PLANS: { id: string; label: string }[] = [
  { id: "basic", label: "Basic" },
  { id: "pro", label: "Pro" },
  { id: "business", label: "Business" },
];

const PRESETS = [7, 30, 60, 90];

const REASONS = ["수강생 혜택", "이벤트", "CS 보상", "테스트", "기타"];

/** `YYYY-MM-DD` in Seoul, `days` from today. The server reads these as Seoul days. */
function seoulDate(days = 0): string {
  const t = Date.now() + 9 * 3600_000 + days * 86_400_000;
  return new Date(t).toISOString().slice(0, 10);
}

/** How a grant reads on a list: what it is, and that an operator gave it. */
export function GrantBadge({ u }: { u: AdminUserRow }) {
  if (!u.grant_plan_id) return null;
  const left = u.grant_days_left ?? 0;
  return (
    <Tag tone={left <= 7 ? "warn" : "live"}>
      {u.grant_plan_id === u.effective_plan_id
        ? `관리자 지급 · D-${Math.max(0, left)}`
        : `${u.grant_plan_id} 지급 · D-${Math.max(0, left)}`}
    </Tag>
  );
}

/** The state of one grant, in a word. */
export function GrantStateTag({ g }: { g: Grant }) {
  const tone =
    g.state === "active" ? "ok" : g.state === "scheduled" ? "warn" : "default";
  const label =
    g.state === "active"
      ? `활성 · D-${Math.max(0, g.days_left)}`
      : g.state === "scheduled"
        ? "시작 전"
        : g.state === "expired"
          ? "만료"
          : "회수됨";
  return <Tag tone={tone}>{label}</Tag>;
}

/**
 * Hand a plan to the accounts an operator selected.
 *
 * The count is in the sentence at the bottom and in the button, because a bulk
 * action's one real risk is doing it to more people than intended. When some of
 * the selection already hold a grant, that is said before anything happens and
 * the operator chooses what it means — silently overwriting somebody's term is
 * how an account ends up with a month it did not get.
 */
export function GrantModal({
  users,
  onCancel,
  onDone,
}: {
  users: AdminUserRow[];
  onCancel: () => void;
  onDone: (r: { plan_label: string; outcomes: GrantOutcome[] }) => void;
}) {
  const [plan, setPlan] = useState("business");
  const [preset, setPreset] = useState<number | "custom">(30);
  const [from, setFrom] = useState(seoulDate());
  const [to, setTo] = useState(seoulDate(30));
  const [reason, setReason] = useState(REASONS[0]!);
  const [other, setOther] = useState("");
  const [onExisting, setOnExisting] = useState<"extend" | "reset">("extend");
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const held = users.filter((u) => u.grant_plan_id);
  const fresh = users.length - held.length;
  const planLabel = PLANS.find((p) => p.id === plan)?.label ?? plan;
  const reasonText = reason === "기타" ? other.trim() : reason;
  const termText = preset === "custom" ? `${from} ~ ${to}` : `${preset}일간`;
  const ready = users.length > 0 && reasonText.length > 0;

  const submit = async () => {
    setBusy(true);
    setFailed(null);
    try {
      const r = await adminApi.createGrants({
        user_ids: users.map((u) => u.id),
        plan_id: plan,
        ...(preset === "custom" ? { from, to } : { days: preset }),
        reason: reasonText,
        on_existing: onExisting,
      });
      onDone(r);
    } catch (e) {
      setFailed(e instanceof Error ? e.message : "지급하지 못했습니다.");
      setConfirming(false);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-start justify-center overflow-y-auto bg-black/60 p-4"
      role="dialog"
      aria-modal="true"
      aria-label="관리자 이용권 지급"
    >
      <div className="my-8 w-full max-w-lg rounded-lg border border-ink-700 bg-ink-900 p-5">
        <h3 className="text-sm font-semibold text-ink-100">관리자 이용권 지급</h3>
        <p className="mt-1 text-[11px] text-ink-500">
          유료 결제와 별개로 기록됩니다. 매출·MRR·결제 건수에 포함되지 않습니다.
        </p>

        {failed && (
          <p role="alert" className="mt-3 rounded-md border border-live-dim bg-ink-950 px-3 py-2 text-sm text-live">
            {failed}
          </p>
        )}

        {confirming ? (
          <div className="mt-4 space-y-3 text-sm text-ink-300">
            <p className="text-ink-100">
              선택한 {users.length}명에게 {planLabel} 이용권을 {termText} 지급합니다.
            </p>
            <ul className="space-y-1 text-xs text-ink-400">
              <li>· 신규 지급 {fresh}명</li>
              {held.length > 0 && (
                <li>
                  · 기존 이용권 보유 {held.length}명 —{" "}
                  {onExisting === "extend" ? "기존 만료일부터 연장" : "오늘부터 새 기간으로 재설정"}
                </li>
              )}
              <li>· 사유: {reasonText}</li>
            </ul>
            {held.length > 0 && (
              <div className="max-h-28 overflow-y-auto rounded border border-ink-700 p-2 text-[11px] text-ink-500">
                {held.map((u) => (
                  <div key={u.id}>
                    {u.email} — {u.grant_plan_id} D-{Math.max(0, u.grant_days_left ?? 0)}
                  </div>
                ))}
              </div>
            )}
            <div className="flex justify-end gap-2 pt-1">
              <button
                onClick={() => setConfirming(false)}
                disabled={busy}
                className="rounded-md border border-ink-600 px-3 py-1.5 text-sm text-ink-300 hover:bg-ink-800"
              >
                뒤로
              </button>
              <button
                onClick={submit}
                disabled={busy}
                className="rounded-md bg-ok px-3 py-1.5 text-sm font-semibold text-ink-950 disabled:opacity-50"
              >
                {busy ? "지급 중…" : `${users.length}명에게 지급`}
              </button>
            </div>
          </div>
        ) : (
          <div className="mt-4 space-y-4">
            <Field label="요금제">
              <div className="flex flex-wrap gap-1.5">
                {PLANS.map((p) => (
                  <Choice key={p.id} on={plan === p.id} onClick={() => setPlan(p.id)}>
                    {p.label}
                  </Choice>
                ))}
              </div>
            </Field>

            <Field label="기간">
              <div className="flex flex-wrap gap-1.5">
                {PRESETS.map((d) => (
                  <Choice
                    key={d}
                    on={preset === d}
                    onClick={() => {
                      setPreset(d);
                      setTo(seoulDate(d));
                    }}
                  >
                    {d}일
                  </Choice>
                ))}
                <Choice on={preset === "custom"} onClick={() => setPreset("custom")}>
                  직접 설정
                </Choice>
              </div>
              {preset === "custom" && (
                <div className="mt-2 flex flex-wrap items-center gap-2 text-xs text-ink-400">
                  <label className="flex items-center gap-1">
                    시작일
                    <input
                      type="date"
                      aria-label="시작일"
                      value={from}
                      onChange={(e) => setFrom(e.target.value)}
                      className="rounded border border-ink-600 bg-ink-950 px-2 py-1 text-ink-100"
                    />
                  </label>
                  <label className="flex items-center gap-1">
                    만료일
                    <input
                      type="date"
                      aria-label="만료일"
                      value={to}
                      onChange={(e) => setTo(e.target.value)}
                      className="rounded border border-ink-600 bg-ink-950 px-2 py-1 text-ink-100"
                    />
                  </label>
                  <span className="text-ink-600">한국 시간 기준, 만료일 당일까지</span>
                </div>
              )}
            </Field>

            <Field label="지급 사유">
              <div className="flex flex-wrap gap-1.5">
                {REASONS.map((r) => (
                  <Choice key={r} on={reason === r} onClick={() => setReason(r)}>
                    {r}
                  </Choice>
                ))}
              </div>
              {reason === "기타" && (
                <input
                  value={other}
                  onChange={(e) => setOther(e.target.value)}
                  aria-label="사유 직접 입력"
                  placeholder="예: 6기 수강생 혜택"
                  className="mt-2 w-full rounded-md border border-ink-600 bg-ink-950 px-3 py-2 text-sm text-ink-100"
                />
              )}
            </Field>

            {held.length > 0 && (
              <Field label={`이미 이용권이 있는 회원 ${held.length}명`}>
                <div className="flex flex-wrap gap-1.5">
                  <Choice on={onExisting === "extend"} onClick={() => setOnExisting("extend")}>
                    기존 만료일부터 연장
                  </Choice>
                  <Choice on={onExisting === "reset"} onClick={() => setOnExisting("reset")}>
                    오늘부터 새 기간으로
                  </Choice>
                </div>
              </Field>
            )}

            <p className="rounded-md border border-ink-700 bg-ink-950 px-3 py-2 text-sm text-ink-200">
              선택한 {users.length}명에게 {planLabel} 이용권을 {termText} 지급합니다.
              {held.length > 0 && ` (${fresh}명 신규, ${held.length}명 기존 보유)`}
            </p>

            <div className="flex justify-end gap-2">
              <button
                onClick={onCancel}
                className="rounded-md border border-ink-600 px-3 py-1.5 text-sm text-ink-300 hover:bg-ink-800"
              >
                취소
              </button>
              <button
                onClick={() => setConfirming(true)}
                disabled={!ready}
                className="rounded-md bg-ok px-3 py-1.5 text-sm font-semibold text-ink-950 disabled:opacity-40"
              >
                이용권 지급
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="mb-1.5 text-[11px] uppercase tracking-wider text-ink-500">{label}</div>
      {children}
    </div>
  );
}

function Choice({
  on,
  onClick,
  children,
}: {
  on: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      aria-pressed={on}
      className={`rounded-md px-2.5 py-1 text-xs ${
        on ? "bg-ink-100 text-ink-950" : "border border-ink-700 text-ink-300 hover:bg-ink-800"
      }`}
    >
      {children}
    </button>
  );
}

/**
 * Why this account is on the plan it is on.
 *
 * Both sources are shown whenever both exist, because the question an operator
 * actually has is not "what plan" but "why that plan, and what happens when
 * this runs out".
 */
export function EntitlementPanel({
  user,
  grants: all,
  onChanged,
}: {
  user: AdminUserRow;
  grants?: Grant[] | null;
  onChanged: () => void;
}) {
  // Defaulted rather than required: a detail payload from a server that predates
  // this field should leave the panel empty, not blank the whole page.
  const grants = all ?? [];
  const live = grants.find((g) => g.state === "active");
  const [acting, setActing] = useState<null | "extend" | "plan" | "revoke">(null);
  const [days, setDays] = useState(30);
  const [plan, setPlan] = useState("business");
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const run = async () => {
    if (!live) return;
    setBusy(true);
    setFailed(null);
    try {
      if (acting === "extend") await adminApi.extendGrant(live.id, days, reason);
      if (acting === "plan") await adminApi.changeGrantPlan(live.id, plan, reason);
      if (acting === "revoke") await adminApi.revokeGrant(live.id, reason);
      setActing(null);
      setReason("");
      onChanged();
    } catch (e) {
      setFailed(e instanceof Error ? e.message : "처리하지 못했습니다.");
    } finally {
      setBusy(false);
    }
  };

  const paying = user.plan_id !== "none";

  return (
    <Panel title="이용 권한">
      <div className="grid gap-4 sm:grid-cols-2">
        <div>
          <div className="text-[11px] uppercase tracking-wider text-ink-500">현재 최종 이용권</div>
          <div className="mt-1 flex items-center gap-2">
            <span className="font-mono text-lg text-ink-100">{user.effective_plan_label}</span>
            <Tag tone={user.entitlement_source === "grant" ? "live" : user.entitlement_source === "paid" ? "ok" : "default"}>
              {user.entitlement_source === "grant"
                ? "관리자 지급"
                : user.entitlement_source === "paid"
                  ? "유료 결제"
                  : "미구독"}
            </Tag>
          </div>
        </div>
        <div className="text-xs text-ink-400">
          <div className="text-[11px] uppercase tracking-wider text-ink-500">유료 구독</div>
          {paying ? (
            <div className="mt-1">
              {user.plan_label} / {user.billing_status ?? user.subscription_status}
              {user.next_charge_at && <span className="text-ink-600"> / ~{user.next_charge_at}</span>}
            </div>
          ) : (
            <div className="mt-1 text-ink-600">없음</div>
          )}
        </div>
      </div>

      {live && (
        <div className="mt-4 rounded-md border border-ink-700 bg-ink-950 p-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="text-sm text-ink-100">
              관리자 지급 · {live.plan_label} <GrantStateTag g={live} />
            </div>
            <div className="flex flex-wrap gap-1.5">
              <Choice on={acting === "extend"} onClick={() => setActing(acting === "extend" ? null : "extend")}>
                기간 연장
              </Choice>
              <Choice on={acting === "plan"} onClick={() => setActing(acting === "plan" ? null : "plan")}>
                이용권 변경
              </Choice>
              <Choice on={acting === "revoke"} onClick={() => setActing(acting === "revoke" ? null : "revoke")}>
                이용권 회수
              </Choice>
            </div>
          </div>
          <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-[11px] text-ink-500 sm:grid-cols-4">
            <Pair k="지급일" v={localDate(live.created_at)} />
            <Pair k="시작일" v={localDate(live.starts_at)} />
            <Pair k="만료일" v={localDate(live.expires_at)} />
            <Pair k="남은 기간" v={`${Math.max(0, live.days_left)}일`} />
            <Pair k="지급 관리자" v={live.granted_by_email} />
            <Pair k="지급 사유" v={live.reason} />
          </dl>

          {acting && (
            <div className="mt-3 space-y-2 border-t border-ink-700 pt-3">
              {acting === "revoke" && (
                <p className="text-xs text-warn">
                  관리자 지급 {live.plan_label} 이용권을 회수합니다. PayApp 유료 구독은 변경되지 않습니다.
                  {paying
                    ? ` 회수 후 ${user.plan_label} 유료 구독으로 돌아갑니다.`
                    : " 회수 후 미구독 상태가 됩니다."}
                </p>
              )}
              {acting === "extend" && (
                <div className="flex flex-wrap gap-1.5">
                  {PRESETS.map((d) => (
                    <Choice key={d} on={days === d} onClick={() => setDays(d)}>
                      +{d}일
                    </Choice>
                  ))}
                </div>
              )}
              {acting === "plan" && (
                <div className="flex flex-wrap gap-1.5">
                  {PLANS.map((p) => (
                    <Choice key={p.id} on={plan === p.id} onClick={() => setPlan(p.id)}>
                      {p.label}
                    </Choice>
                  ))}
                </div>
              )}
              {failed && (
                <p role="alert" className="text-xs text-live">
                  {failed}
                </p>
              )}
              <label className="block">
                <span className="text-[11px] uppercase tracking-wider text-ink-500">
                  사유 (감사 로그에 남습니다)
                </span>
                <input
                  value={reason}
                  onChange={(e) => setReason(e.target.value)}
                  aria-label="사유"
                  placeholder={acting === "revoke" ? "예: 수강 종료" : "예: 6기 연장"}
                  className="mt-1 w-full rounded-md border border-ink-600 bg-ink-900 px-3 py-2 text-sm text-ink-100"
                />
              </label>
              <div className="flex justify-end gap-2">
                <button
                  onClick={() => setActing(null)}
                  disabled={busy}
                  className="rounded-md border border-ink-600 px-3 py-1.5 text-sm text-ink-300 hover:bg-ink-800"
                >
                  취소
                </button>
                <button
                  onClick={run}
                  disabled={busy || reason.trim().length === 0}
                  className={`rounded-md px-3 py-1.5 text-sm disabled:opacity-40 ${
                    acting === "revoke" ? "bg-live text-white" : "bg-ok font-semibold text-ink-950"
                  }`}
                >
                  {busy ? "처리 중…" : acting === "revoke" ? "회수" : "적용"}
                </button>
              </div>
            </div>
          )}
        </div>
      )}

      {grants.length > 0 && (
        <div className="mt-4">
          <div className="mb-1.5 text-[11px] uppercase tracking-wider text-ink-500">지급 이력</div>
          <Table head={["요금제", "기간", "상태", "사유", "지급 관리자"]}>
            {grants.map((g) => (
              <Row key={g.id}>
                <Cell>{g.plan_label}</Cell>
                <Cell mono>
                  {localDate(g.starts_at)} ~ {localDate(g.expires_at)}
                </Cell>
                <Cell>
                  <GrantStateTag g={g} />
                </Cell>
                <Cell className="max-w-[14rem] truncate text-[11px] text-ink-500">
                  {g.revoke_reason ?? g.reason}
                </Cell>
                <Cell className="text-[11px] text-ink-500">{g.granted_by_email}</Cell>
              </Row>
            ))}
          </Table>
        </div>
      )}
      {grants.length === 0 && (
        <p className="mt-4 text-xs text-ink-600">관리자 지급 이력이 없습니다.</p>
      )}
    </Panel>
  );
}

function Pair({ k, v }: { k: string; v: string }) {
  return (
    <div>
      <dt className="text-ink-600">{k}</dt>
      <dd className="text-ink-300">{v}</dd>
    </div>
  );
}
