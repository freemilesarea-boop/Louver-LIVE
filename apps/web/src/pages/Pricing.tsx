/**
 * The price list.
 *
 * Every number on this page comes from `GET /api/plans`. That is the point of
 * the request: a price hard-coded here would be a second place the service's
 * prices live, and the two would eventually disagree — with the customer reading
 * one and the server charging the other.
 *
 * The "시작하기" buttons do not start anything. There is no payment yet, and
 * there is deliberately no endpoint they could call that would activate a plan:
 * see `CloudDb::activate_subscription`, which only a verified payment or the
 * operator's own CLI can reach.
 */
import { useEffect, useState } from "react";
import { Button, Card } from "@/components/ui";
import { useTransport } from "../TransportContext";
import {
  PLAN_FEATURES,
  RECOMMENDED_PLAN,
  concurrentStreams,
  formatWon,
} from "../cloud";
import type { Plan } from "../cloud";

export function Pricing({ onClose }: { onClose?: () => void }) {
  const t = useTransport();
  const [plans, setPlans] = useState<Plan[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Which plan's "not yet" notice is showing. */
  const [chosen, setChosen] = useState<Plan | null>(null);

  useEffect(() => {
    let live = true;
    t.plans()
      .then((p) => live && setPlans(p))
      .catch(
        () =>
          live &&
          setError("요금제를 불러오지 못했습니다. 잠시 후 다시 시도해주세요."),
      );
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

      {chosen && <NotYet plan={chosen} onDismiss={() => setChosen(null)} />}

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
 * What "시작하기" actually does today.
 *
 * Nothing is sent. A button that quietly activated a plan would be a way to get
 * a paid entitlement for free, so this says what is true instead: the payment
 * system is not built yet.
 */
function NotYet({ plan, onDismiss }: { plan: Plan; onDismiss: () => void }) {
  return (
    <Card title="결제 준비 중">
      <p
        role="status"
        className="text-sm text-ink-100"
        data-testid="payment-coming-soon"
      >
        결제 시스템을 준비 중입니다.
      </p>
      <p className="mt-2 text-sm text-ink-400">
        {plan.label} 요금제({formatWon(plan.monthly_price_krw)} / 월)를
        선택하셨습니다. 결제가 열리면 바로 시작할 수 있도록 안내드리겠습니다.
      </p>
      <div className="mt-4">
        <Button onClick={onDismiss}>확인</Button>
      </div>
    </Card>
  );
}
