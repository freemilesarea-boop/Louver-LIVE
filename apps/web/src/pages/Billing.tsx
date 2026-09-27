/**
 * The two billing screens: what happens after PayApp, and the account's own
 * subscription panel.
 *
 * The rule both of them follow: **the browser is not evidence.** Arriving back
 * from PayApp says only that a person pressed something on PayApp's page. Whether
 * a payment was approved is known to this app only because the server verified a
 * server-to-server notification, so both screens ask the server and believe
 * nothing else.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { Button, Card } from "@/components/ui";
import { Wordmark } from "../App";
import { useTransport } from "../TransportContext";
import { BILLING_STATUS_LABELS, billingIsLive, formatWon } from "../cloud";
import type { BillingStatusResponse } from "../cloud";

/** Is this the page PayApp's `returnurl` points at? */
export function isBillingComplete(pathname: string): boolean {
  return pathname.replace(/\/+$/, "") === "/billing/complete";
}

/** How long to wait for the notification before saying it is late. */
const POLL_LIMIT = 20;
const POLL_EVERY_MS = 1500;

/**
 * `/billing/complete`. §11.
 *
 * PayApp sends the browser here as soon as its own page is done, which may be
 * before its server has told ours anything. So this polls — briefly, and with an
 * end — and never concludes success from the fact that it was loaded.
 */
export function BillingComplete({ onDone }: { onDone?: () => void }) {
  const t = useTransport();
  const [status, setStatus] = useState<BillingStatusResponse | null>(null);
  const [tries, setTries] = useState(0);
  const [gaveUp, setGaveUp] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let live = true;
    let attempt = 0;

    const ask = async () => {
      try {
        const next = await t.billingStatus();
        if (!live) return;
        setStatus(next);
        // The server says the entitlement is active: the notification arrived and
        // was verified. Nothing else counts as done.
        if (next.plan.active) return;
      } catch {
        /* a blip; the next tick asks again */
      }
      if (!live) return;
      attempt += 1;
      setTries(attempt);
      if (attempt >= POLL_LIMIT) {
        setGaveUp(true);
        return;
      }
      timer.current = setTimeout(ask, POLL_EVERY_MS);
    };
    ask();

    return () => {
      live = false;
      if (timer.current) clearTimeout(timer.current);
    };
  }, [t]);

  const done = status?.plan.active === true;

  return (
    <div className="flex min-h-screen items-center justify-center bg-ink-950 p-6">
      <div className="w-full max-w-md">
        <h1 className="mb-6 text-center text-2xl">
          <Wordmark />
        </h1>
        <Card title="결제">
          {done ? (
            <div data-testid="billing-complete-done">
              <p className="text-sm text-ok">결제가 완료되었습니다.</p>
              <p className="mt-2 text-sm text-ink-100">
                현재 요금제:{" "}
                <span className="font-semibold">
                  {status?.plan.plan?.label}
                </span>
              </p>
              <p className="mt-1 text-xs text-ink-500">
                동시 송출 {status?.plan.limits?.max_concurrent_streams ?? 0}개를
                사용할 수 있습니다.
              </p>
            </div>
          ) : gaveUp ? (
            <div data-testid="billing-complete-slow">
              <p className="text-sm text-warn">
                결제 확인이 지연되고 있습니다.
              </p>
              <p className="mt-2 text-sm text-ink-400">
                잠시 후 다시 확인해주세요. 결제가 승인되었다면 요금제는 자동으로
                활성화됩니다.
              </p>
            </div>
          ) : (
            <div data-testid="billing-complete-waiting">
              <p className="text-sm text-ink-100">
                결제 결과를 확인하고 있습니다.
              </p>
              <p className="mt-2 text-xs text-ink-500">
                PayApp의 확인을 기다리는 중입니다… ({tries}/{POLL_LIMIT})
              </p>
            </div>
          )}

          <div className="mt-5 flex gap-2">
            <Button
              variant="primary"
              onClick={() => {
                if (onDone) onDone();
                else window.location.assign("/");
              }}
            >
              대시보드로
            </Button>
            {!done && !gaveUp && (
              <Button onClick={() => setGaveUp(true)}>
                기다리지 않고 나가기
              </Button>
            )}
          </div>
        </Card>
      </div>
    </div>
  );
}

/**
 * The subscription panel. §12.
 *
 * Shows the plan, the provider's state and the monthly amount, and offers a
 * cancellation that takes effect at once: the recurring payment stops and the paid
 * features go with it. That is what the confirmation has to say in so many words —
 * the previous wording promised the rest of the paid period, which the server no
 * longer honours, and a promise the server breaks is worse than no promise.
 */
export function BillingPanel({ onSeePricing }: { onSeePricing?: () => void }) {
  const t = useTransport();
  const [status, setStatus] = useState<BillingStatusResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setStatus(await t.billingStatus());
    } catch {
      /* shown by the next action that fails */
    }
  }, [t]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  async function cancel() {
    setBusy(true);
    setError(null);
    try {
      await t.cancelBilling();
      setConfirming(false);
      await refresh();
    } catch (e) {
      setError(
        e instanceof Error
          ? e.message
          : "해지하지 못했습니다. 잠시 후 다시 시도해주세요.",
      );
    } finally {
      setBusy(false);
    }
  }

  const sub = status?.subscription ?? null;
  const plan = status?.plan;
  const cancellable = sub !== null && billingIsLive(sub);

  return (
    <Card
      title="구독"
      action={
        !cancellable && onSeePricing ? (
          <Button size="sm" variant="primary" onClick={onSeePricing}>
            요금제 보기
          </Button>
        ) : undefined
      }
    >
      {error && (
        <p role="alert" className="mb-3 text-sm text-live">
          {error}
        </p>
      )}

      <dl className="grid gap-y-2 text-sm">
        <Row
          label="현재 요금제"
          value={plan?.active ? (plan.plan?.label ?? "—") : "미구독"}
        />
        <Row
          label="구독 상태"
          value={
            sub
              ? BILLING_STATUS_LABELS[sub.status]
              : plan?.active
                ? "구독 중"
                : "미구독"
          }
        />
        <Row
          label="월 결제 금액"
          value={
            sub
              ? `${formatWon(sub.amount_krw)} / 월`
              : plan?.plan
                ? `${formatWon(plan.plan.monthly_price_krw)} / 월`
                : "—"
          }
        />
        <Row
          label="결제 제공자"
          value={
            sub ? "PayApp" : status?.configured ? "PayApp" : "설정되지 않음"
          }
        />
        {sub?.last_paid_at && (
          <Row label="최근 결제" value={sub.last_paid_at} />
        )}
      </dl>

      {(sub?.status === "cancelled" || sub?.status === "cancel_at_period_end") && (
        <p className="mt-3 text-xs text-warn" data-testid="cancel-done">
          구독이 해지되었습니다. 자동결제가 중단되었으며 유료 기능은 사용할 수
          없습니다. 다시 이용하시려면 요금제를 새로 결제해주세요.
        </p>
      )}
      {sub?.status === "payment_failed" && (
        <p className="mt-3 text-xs text-warn">
          최근 결제가 실패했습니다. 결제 수단을 확인해주세요. 기존 이용은 계속
          유지됩니다.
        </p>
      )}
      {sub?.status === "pending" && (
        <p className="mt-3 text-xs text-ink-500">
          첫 결제가 아직 승인되지 않았습니다. 승인되면 요금제가 자동으로
          활성화됩니다.
        </p>
      )}

      {cancellable &&
        (confirming ? (
          <div className="mt-4 rounded-md border border-ink-700 bg-ink-900 p-3">
            <p className="text-sm text-ink-100">구독을 해지하시겠습니까?</p>
            <p className="mt-1 text-xs text-ink-400">
              구독을 취소하면 자동결제가 해지되며 247streams 유료 기능을 즉시
              사용할 수 없게 됩니다. 진행 중인 방송이 있다면 먼저 확인해주세요.
            </p>
            <div className="mt-3 flex gap-2">
              <Button
                variant="danger"
                size="sm"
                onClick={cancel}
                disabled={busy}
              >
                {busy ? "해지 중…" : "해지하기"}
              </Button>
              <Button
                size="sm"
                onClick={() => setConfirming(false)}
                disabled={busy}
              >
                유지하기
              </Button>
            </div>
          </div>
        ) : (
          <div className="mt-4">
            <Button size="sm" onClick={() => setConfirming(true)}>
              구독 해지
            </Button>
          </div>
        ))}
    </Card>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-4 border-b border-ink-800 py-1">
      <dt className="text-ink-500">{label}</dt>
      <dd className="text-ink-100">{value}</dd>
    </div>
  );
}
