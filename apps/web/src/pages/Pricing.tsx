/**
 * The price list.
 *
 * Every number on this page comes from `GET /api/plans`. That is the point of
 * the request: a price hard-coded here would be a second place the service's
 * prices live, and the two would eventually disagree — with the customer reading
 * one and the server charging the other.
 *
 * Pressing 시작하기 asks the server for a PayApp payment URL and sends the browser
 * there. It does not activate anything: only a verified PayApp notification does,
 * and no route a browser can reach touches `activate_subscription`.
 */
import { useEffect, useState } from "react";
import { Button, Card, Field, Input, Modal } from "@/components/ui";
import { useTransport } from "../TransportContext";
import {
  PLAN_FEATURES,
  RECOMMENDED_PLAN,
  billingIsLive,
  concurrentStreams,
  formatWon,
} from "../cloud";
import type { BillingSubscription, Plan } from "../cloud";

export function Pricing({ onClose }: { onClose?: () => void }) {
  const t = useTransport();
  const [plans, setPlans] = useState<Plan[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Which plan's checkout is open. */
  const [chosen, setChosen] = useState<Plan | null>(null);
  /** Can this deployment take a payment, and is one already running? */
  const [configured, setConfigured] = useState(true);
  const [existing, setExisting] = useState<BillingSubscription | null>(null);

  useEffect(() => {
    let live = true;
    t.plans()
      .then((p) => live && setPlans(p))
      .catch(
        () =>
          live &&
          setError("요금제를 불러오지 못했습니다. 잠시 후 다시 시도해주세요."),
      );
    // Signed out, or a desktop build: the price list still renders, and the
    // buttons say what is possible instead of failing when pressed.
    t.billingStatus()
      .then((b) => {
        if (!live) return;
        setConfigured(b.configured);
        setExisting(
          b.subscription && billingIsLive(b.subscription)
            ? b.subscription
            : null,
        );
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [t]);

  return (
    <div className="mx-auto max-w-4xl">
      <header className="mb-6 text-center">
        <h1 className="text-xl font-semibold text-ink-100">요금제</h1>
        <p className="mt-2 text-sm text-ink-400">
          필요한 만큼만 사용하세요.
          <br />
          모든 요금제에서 24시간 클라우드 송출을 사용할 수 있습니다.
        </p>
      </header>

      {error && (
        <p role="alert" className="mb-4 text-center text-sm text-live">
          {error}
        </p>
      )}

      {plans === null && !error ? (
        <p className="text-center text-sm text-ink-500">불러오는 중…</p>
      ) : (
        /* One column on a phone, three from `sm` up. */
        <div className="grid gap-4 sm:grid-cols-3" data-testid="pricing-grid">
          {(plans ?? []).map((p) => (
            <PlanCard key={p.id} plan={p} onChoose={() => setChosen(p)} />
          ))}
        </div>
      )}

      {/* §14: one recurring registration per account. Said here rather than
          discovered when the server refuses. */}
      {existing && (
        <p
          className="mt-4 text-center text-sm text-warn"
          data-testid="already-subscribed"
        >
          이미 정기결제가 등록되어 있습니다. 현재 구독을 해지한 후 요금제를
          변경할 수 있습니다.
        </p>
      )}

      {chosen &&
        (configured && !existing ? (
          <CheckoutModal plan={chosen} onClose={() => setChosen(null)} />
        ) : (
          <NotReady
            plan={chosen}
            blocked={existing}
            onDismiss={() => setChosen(null)}
          />
        ))}

      {onClose && (
        <p className="mt-6 text-center">
          <button
            type="button"
            onClick={onClose}
            className="text-xs text-ink-400 hover:text-ink-100"
          >
            돌아가기
          </button>
        </p>
      )}
    </div>
  );
}

function PlanCard({ plan, onChoose }: { plan: Plan; onChoose: () => void }) {
  const recommended = plan.id === RECOMMENDED_PLAN;
  const streams = concurrentStreams(plan);

  return (
    <div
      data-testid="plan-card"
      data-plan={plan.id}
      className={`rounded-lg border bg-ink-900 p-5 ${
        recommended ? "border-ok" : "border-ink-700"
      }`}
    >
      <div className="flex items-baseline justify-between">
        <h2 className="text-sm font-semibold uppercase tracking-wide text-ink-100">
          {plan.label}
        </h2>
        {recommended && (
          <span className="rounded-full bg-ok/10 px-2 py-0.5 text-[11px] text-ok">
            추천
          </span>
        )}
      </div>

      <p className="mt-3">
        <span
          className="text-2xl font-semibold text-ink-100"
          data-testid="price"
        >
          {formatWon(plan.monthly_price_krw)}
        </span>
        <span className="ml-1 text-xs text-ink-500">/ 월</span>
      </p>
      <p className="mt-1 text-xs text-ink-400">{plan.description}</p>

      {/* The one entitlement that differs between plans, said plainly. */}
      <p
        className="mt-4 rounded-md border border-ink-700 bg-ink-850 px-3 py-2 text-sm text-ink-100"
        data-testid="concurrency"
      >
        동시 송출 {streams}개
      </p>

      <ul className="mt-4 space-y-1.5 text-sm text-ink-400">
        {PLAN_FEATURES.map((f) => (
          <li key={f} className="flex gap-2">
            <span aria-hidden="true" className="text-ok">
              ✓
            </span>
            <span>{f}</span>
          </li>
        ))}
      </ul>

      <Button
        variant={recommended ? "primary" : "ghost"}
        className="mt-5 w-full"
        onClick={onChoose}
      >
        {plan.label} 시작하기
      </Button>
    </div>
  );
}

/**
 * The checkout. §10.
 *
 * Collects the one thing PayApp requires and we do not already know — the mobile
 * number the payment link is sent to — and then hands the browser to PayApp.
 *
 * The price shown is for reading. What is charged is whatever the server's plans
 * table says, and this component sends no amount at all.
 */
function CheckoutModal({ plan, onClose }: { plan: Plan; onClose: () => void }) {
  const t = useTransport();
  const [phone, setPhone] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const digits = phone.replace(/\D/g, "");
  const usable =
    digits.length >= 10 && digits.length <= 11 && digits.startsWith("01");

  async function start() {
    if (busy) return;
    if (!usable) {
      setError("휴대폰 번호를 확인해주세요. (예: 01012345678)");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const { payurl } = await t.startCheckout(plan.id, digits);
      // PayApp's page, not ours. Nothing is subscribed until their server tells
      // ours that the first payment was approved.
      window.location.assign(payurl);
    } catch (e) {
      setError(
        e instanceof Error
          ? e.message
          : "결제를 시작할 수 없습니다. 잠시 후 다시 시도해주세요.",
      );
      setBusy(false);
    }
  }

  return (
    <Modal
      open
      title="정기결제 시작"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            취소
          </Button>
          <Button variant="primary" onClick={start} disabled={busy}>
            {busy ? "결제 준비 중…" : "정기결제 시작"}
          </Button>
        </>
      }
    >
      <dl className="grid gap-y-2 text-sm" data-testid="checkout-summary">
        <div className="flex justify-between border-b border-ink-800 py-1">
          <dt className="text-ink-500">선택 요금제</dt>
          <dd className="text-ink-100">{plan.label}</dd>
        </div>
        <div className="flex justify-between border-b border-ink-800 py-1">
          <dt className="text-ink-500">월 가격</dt>
          <dd className="text-ink-100">
            {formatWon(plan.monthly_price_krw)} / 월
          </dd>
        </div>
        <div className="flex justify-between border-b border-ink-800 py-1">
          <dt className="text-ink-500">동시 송출</dt>
          <dd className="text-ink-100">{concurrentStreams(plan)}개</dd>
        </div>
      </dl>

      <Field
        label="휴대폰 번호"
        hint="결제 링크를 받을 번호입니다. 카드 정보는 PayApp 결제창에서 직접 입력합니다."
      >
        <Input
          type="tel"
          inputMode="numeric"
          autoComplete="tel"
          aria-label="휴대폰 번호"
          placeholder="01012345678"
          value={phone}
          onChange={(e) => setPhone(e.target.value)}
        />
      </Field>

      <p className="text-xs text-ink-500">
        다음 화면에서 PayApp 결제창이 열립니다. 첫 결제가 승인되면 매월 같은 날
        자동으로 결제됩니다. 언제든 해지할 수 있습니다.
      </p>

      {error && (
        <p role="alert" className="pt-2 text-sm text-live">
          {error}
        </p>
      )}
    </Modal>
  );
}

/**
 * Why a checkout cannot start: either this deployment has no PayApp credentials,
 * or the account already has a registration PayApp is holding.
 */
function NotReady({
  plan,
  blocked,
  onDismiss,
}: {
  plan: Plan;
  blocked: BillingSubscription | null;
  onDismiss: () => void;
}) {
  return (
    <Card title={blocked ? "이미 구독 중입니다" : "결제 준비 중"}>
      <p
        role="status"
        className="text-sm text-ink-100"
        data-testid="checkout-unavailable"
      >
        {blocked
          ? "현재 구독을 해지한 후 요금제를 변경할 수 있습니다."
          : "결제 시스템을 준비 중입니다."}
      </p>
      <p className="mt-2 text-sm text-ink-400">
        {plan.label} 요금제({formatWon(plan.monthly_price_krw)} / 월)를
        선택하셨습니다.
      </p>
      <div className="mt-4">
        <Button onClick={onDismiss}>확인</Button>
      </div>
    </Card>
  );
}
