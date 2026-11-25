/**
 * The price list.
 *
 * Every number on this page comes from `GET /api/plans`. That is the point of
 * the request: a price hard-coded here would be a second place the service's
 * prices live, and the two would eventually disagree — with the customer reading
 * one and the server charging the other.
 *
 * Enterprise is the one card whose price is not in that list, because it is
 * priced per channel and a card per channel count is not a price list. Its
 * figure comes from `GET /api/plans/enterprise?channels=N` — a *quote*, asked
 * for every time the number changes. This file never multiplies 19,000 by
 * anything: if it did, that would be the second place prices live that the
 * paragraph above exists to prevent.
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
  cctvEnabled,
  concurrentStreams,
  formatPlanGb,
  formatWon,
  storageLimit,
  uploadLimit,
} from "../cloud";
import type {
  BillingSubscription,
  Checkout,
  EnterpriseQuote,
  Plan,
} from "../cloud";

/**
 * A checkout waiting for a phone number.
 *
 * Built where the button is pressed, so one modal serves both a listed plan and
 * an Enterprise quote: the two differ only in which transport call `start` is,
 * and the modal does not need to know which it was handed.
 */
interface Choice {
  label: string;
  priceKrw: number;
  streams: number;
  start: (digits: string) => Promise<Checkout>;
}

export function Pricing({ onClose }: { onClose?: () => void }) {
  const t = useTransport();
  const [plans, setPlans] = useState<Plan[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Which checkout is open, if any. */
  const [chosen, setChosen] = useState<Choice | null>(null);
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
            <PlanCard
              key={p.id}
              plan={p}
              onChoose={() =>
                setChosen({
                  label: p.label,
                  priceKrw: p.monthly_price_krw,
                  streams: concurrentStreams(p),
                  start: (digits) => t.startCheckout(p.id, digits),
                })
              }
            />
          ))}
        </div>
      )}

      {/* Full width under the three, because it is not one of them: the count
          is typed, and the price comes back from the server. */}
      <EnterpriseCard
        onChoose={(q) =>
          setChosen({
            label: q.label,
            priceKrw: q.monthly_price_krw,
            streams: concurrentStreams(q),
            start: (digits) => t.startEnterpriseCheckout(q.channels, digits),
          })
        }
      />

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
          <CheckoutModal choice={chosen} onClose={() => setChosen(null)} />
        ) : (
          <NotReady
            choice={chosen}
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
  const storage = storageLimit(plan);
  const perFile = uploadLimit(plan);
  const cctv = cctvEnabled(plan);

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

      {/* Also from the payload: what the server enforces is what is shown. */}
      {storage > 0 && perFile > 0 && (
        <p className="mt-2 text-xs text-ink-400" data-testid="storage">
          저장공간 {formatPlanGb(storage)} · 파일당 최대 {formatPlanGb(perFile)}
        </p>
      )}

      {/* Only on the plans that carry it, so this reads as what sets them
          apart rather than as a line every card repeats. */}
      {cctv && (
        <p className="mt-2 text-xs text-ok" data-testid="cctv">
          CCTV 트래픽 이용 가능
        </p>
      )}

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
 * Enterprise: 4채널 이상, 채널당 정액.
 *
 * The only card where the customer names the size. Two things keep that honest:
 *
 * - **The price is quoted, never computed.** Every change of the number asks
 *   the server what that many channels cost, and what comes back is what is
 *   rendered. So this component has no rate in it to drift away from the one
 *   the server charges.
 * - **The count is what the checkout sends.** Not the price. The server turns
 *   the count into a plan row and reads the amount back out of it, exactly as
 *   it does for Basic, Pro and Business.
 *
 * If the quote cannot be fetched the card removes itself, which is also how the
 * desktop build — where nothing is on sale — ends up not showing it.
 */
function EnterpriseCard({
  onChoose,
}: {
  onChoose: (quote: EnterpriseQuote) => void;
}) {
  const t = useTransport();
  /** The entry-level quote. Where the minimum and the per-channel rate come
   *  from, and it stays put while a new count is being quoted. */
  const [base, setBase] = useState<EnterpriseQuote | null>(null);
  /** The quote for what is typed now. `null` while in flight or unusable. */
  const [quote, setQuote] = useState<EnterpriseQuote | null>(null);
  const [count, setCount] = useState("");
  const [note, setNote] = useState<string | null>(null);
  const [gone, setGone] = useState(false);

  useEffect(() => {
    let live = true;
    t.enterpriseQuote()
      .then((q) => {
        if (!live) return;
        setBase(q);
        setQuote(q);
        setCount(String(q.channels));
      })
      .catch(() => live && setGone(true));
    return () => {
      live = false;
    };
  }, [t]);

  const min = base?.min_channels ?? null;

  // Re-quote whatever is in the box. Debounced, because somebody typing "12"
  // passes through "1" and that is not a question worth asking the server.
  useEffect(() => {
    if (min === null) return;
    const typed = count.trim();
    if (!/^\d+$/.test(typed)) {
      setQuote(null);
      setNote("채널 갯수를 숫자로 입력해주세요.");
      return;
    }
    const n = Number(typed);
    if (n < min) {
      setQuote(null);
      setNote(`엔터프라이즈는 ${min}채널부터입니다.`);
      return;
    }
    setNote(null);
    let live = true;
    const timer = setTimeout(() => {
      t.enterpriseQuote(n)
        .then((q) => live && setQuote(q))
        .catch((e) => {
          if (!live) return;
          setQuote(null);
          setNote(
            e instanceof Error
              ? e.message
              : "금액을 확인할 수 없습니다. 잠시 후 다시 시도해주세요.",
          );
        });
    }, 250);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [t, count, min]);

  if (gone || !base) return null;

  const storage = quote ? storageLimit(quote) : 0;
  const perFile = quote ? uploadLimit(quote) : 0;

  return (
    <div
      data-testid="enterprise-card"
      className="mt-4 rounded-lg border border-ink-700 bg-ink-900 p-5"
    >
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h2 className="text-sm font-semibold uppercase tracking-wide text-ink-100">
          Enterprise
        </h2>
        <span className="text-xs text-ink-400">
          {base.min_channels}채널 이상 · 채널당{" "}
          {formatWon(base.price_per_channel_krw)} / 월
        </span>
      </div>

      <p className="mt-1 text-xs text-ink-400">
        채널을 많이 운영할수록 채널당 가격이 내려갑니다. 필요한 갯수를 입력하면
        매월 자동으로 결제됩니다.
      </p>

      <div className="mt-4 flex flex-wrap items-end gap-4">
        <div className="w-32">
          <Field label="채널 갯수">
            <Input
              type="number"
              inputMode="numeric"
              min={base.min_channels}
              step={1}
              aria-label="채널 갯수"
              value={count}
              onChange={(e) => setCount(e.target.value)}
            />
          </Field>
        </div>
        <p className="pb-1">
          <span
            className="text-2xl font-semibold text-ink-100"
            data-testid="enterprise-price"
          >
            {quote ? formatWon(quote.monthly_price_krw) : "—"}
          </span>
          <span className="ml-1 text-xs text-ink-500">/ 월</span>
        </p>
      </div>

      {/* Straight from the quote, like every other number on this page. */}
      {quote && (
        <>
          <p
            className="mt-2 rounded-md border border-ink-700 bg-ink-850 px-3 py-2 text-sm text-ink-100"
            data-testid="enterprise-concurrency"
          >
            동시 송출 {concurrentStreams(quote)}개
          </p>
          {storage > 0 && perFile > 0 && (
            <p
              className="mt-2 text-xs text-ink-400"
              data-testid="enterprise-storage"
            >
              저장공간 {formatPlanGb(storage)} · 파일당 최대{" "}
              {formatPlanGb(perFile)}
            </p>
          )}
          {cctvEnabled(quote) && (
            <p className="mt-2 text-xs text-ok" data-testid="enterprise-cctv">
              CCTV 트래픽 이용 가능
            </p>
          )}
        </>
      )}

      <p className="mt-3 text-sm text-ink-400">
        Business 요금제의 모든 기능을 포함합니다.
      </p>

      {/* Self-serve is the normal path — a count and a card, no waiting on
          anybody. This is only for what a number cannot express. There is no
          support address in the app yet, so it says who to ask rather than
          inventing a channel. */}
      <p className="mt-1 text-xs text-ink-500" data-testid="enterprise-contact">
        더 많은 채널이나 별도 구성이 필요하시면 247streams 운영자에게
        문의해주세요.
      </p>

      {note && (
        <p
          role="status"
          className="mt-3 text-sm text-warn"
          data-testid="enterprise-note"
        >
          {note}
        </p>
      )}

      <Button
        variant="primary"
        className="mt-5 w-full sm:w-auto"
        disabled={!quote}
        onClick={() => quote && onChoose(quote)}
      >
        엔터프라이즈 시작하기
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
function CheckoutModal({
  choice,
  onClose,
}: {
  choice: Choice;
  onClose: () => void;
}) {
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
      const { payurl } = await choice.start(digits);
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
          <dd className="text-ink-100">{choice.label}</dd>
        </div>
        <div className="flex justify-between border-b border-ink-800 py-1">
          <dt className="text-ink-500">월 가격</dt>
          <dd className="text-ink-100">{formatWon(choice.priceKrw)} / 월</dd>
        </div>
        <div className="flex justify-between border-b border-ink-800 py-1">
          <dt className="text-ink-500">동시 송출</dt>
          <dd className="text-ink-100">{choice.streams}개</dd>
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
  choice,
  blocked,
  onDismiss,
}: {
  choice: Choice;
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
        {choice.label} 요금제({formatWon(choice.priceKrw)} / 월)를
        선택하셨습니다.
      </p>
      <div className="mt-4">
        <Button onClick={onDismiss}>확인</Button>
      </div>
    </Card>
  );
}
