/**
 * The pieces every admin screen is built from.
 *
 * Three of them exist only to make the same three states impossible to forget:
 * a screen that is loading, a screen with nothing on it, and a screen whose
 * request failed. An operations console that shows an empty table for all three
 * is a console that lies during an incident.
 */
import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { AdminError, won, wonShort } from "./api";

export function Loading({ what = "불러오는 중…" }: { what?: string }) {
  return (
    <div className="flex items-center gap-2 py-10 text-sm text-ink-500" role="status">
      <span className="h-2 w-2 animate-pulse rounded-full bg-ink-500" />
      {what}
    </div>
  );
}

export function Empty({ title, hint }: { title: string; hint?: string }) {
  return (
    <div className="py-10 text-center">
      <p className="text-sm text-ink-400">{title}</p>
      {hint && <p className="mt-1 text-xs text-ink-600">{hint}</p>}
    </div>
  );
}

export function Failed({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const message =
    error instanceof AdminError
      ? error.message
      : error instanceof Error
        ? error.message
        : "데이터를 불러오지 못했습니다.";
  return (
    <div role="alert" className="rounded-md border border-live-dim bg-ink-900 px-4 py-6 text-center">
      <p className="text-sm text-live">{message}</p>
      {onRetry && (
        <button
          onClick={onRetry}
          className="mt-3 rounded-md border border-ink-600 px-3 py-1.5 text-xs text-ink-300 hover:bg-ink-800"
        >
          다시 시도
        </button>
      )}
    </div>
  );
}

/**
 * Load something, and hand back the three states rather than one value.
 *
 * Every screen below uses it, which is what makes "loading, empty, error" a
 * property of the console rather than a thing each page remembers to do.
 */
export function useLoad<T>(load: () => Promise<T>, deps: unknown[] = []) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(true);
  const [tick, setTick] = useState(0);

  // The loader is a fresh closure on every render, so it is held in a ref and
  // never becomes a dependency. What decides when to ask again is the caller's
  // own deps, compared by value — they are the search term, the filter, the
  // page offset, and comparing those by identity would reload on every
  // keystroke in an unrelated field.
  const latest = useRef(load);
  latest.current = load;
  const key = JSON.stringify(deps);

  useEffect(() => {
    let live = true;
    setBusy(true);
    latest
      .current()
      .then((d) => live && (setData(d), setError(null)))
      .catch((e) => live && setError(e))
      .finally(() => live && setBusy(false));
    return () => {
      live = false;
    };
  }, [key, tick]);

  return { data, error, busy, reload: () => setTick((t) => t + 1) };
}

/** A headline number. The dashboard is mostly these. */
export function Kpi({
  label,
  value,
  sub,
  tone = "default",
}: {
  label: string;
  value: ReactNode;
  sub?: ReactNode;
  tone?: "default" | "ok" | "warn" | "live";
}) {
  const tones = {
    default: "text-ink-100",
    ok: "text-ok",
    warn: "text-warn",
    live: "text-live",
  };
  return (
    <div className="rounded-lg border border-ink-700 bg-ink-850 px-4 py-3">
      <div className="text-[11px] uppercase tracking-wider text-ink-500">{label}</div>
      <div className={`mt-1 font-mono text-xl ${tones[tone]}`}>{value}</div>
      {sub && <div className="mt-1 text-[11px] text-ink-500">{sub}</div>}
    </div>
  );
}

/** Month on month, when there is a month to compare with. */
export function Change({ pct }: { pct: number | null }) {
  if (pct === null) return <span className="text-ink-600">지난달 없음</span>;
  const up = pct >= 0;
  return (
    <span className={up ? "text-ok" : "text-live"}>
      {up ? "▲" : "▼"} {Math.abs(pct).toFixed(1)}%
    </span>
  );
}

export function Panel({
  title,
  action,
  children,
}: {
  title: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="rounded-lg border border-ink-700 bg-ink-850">
      <header className="flex items-center justify-between gap-3 border-b border-ink-700 px-4 py-2.5">
        <h2 className="text-xs font-semibold uppercase tracking-widest text-ink-400">{title}</h2>
        {action}
      </header>
      <div className="p-4">{children}</div>
    </section>
  );
}

/** A table that scrolls sideways on a phone instead of breaking the page. */
export function Table({ head, children }: { head: string[]; children: ReactNode }) {
  return (
    <div className="-mx-4 overflow-x-auto px-4">
      <table className="w-full min-w-[42rem] border-collapse text-sm">
        <thead>
          <tr className="border-b border-ink-700 text-left text-[11px] uppercase tracking-wider text-ink-500">
            {head.map((h) => (
              <th key={h} className="whitespace-nowrap py-2 pr-4 font-medium">
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>{children}</tbody>
      </table>
    </div>
  );
}

export function Row({ children, onClick }: { children: ReactNode; onClick?: () => void }) {
  return (
    <tr
      onClick={onClick}
      className={`border-b border-ink-800 ${onClick ? "cursor-pointer hover:bg-ink-800" : ""}`}
    >
      {children}
    </tr>
  );
}

export function Cell({
  children,
  className = "",
  mono,
}: {
  children: ReactNode;
  className?: string;
  mono?: boolean;
}) {
  return (
    <td className={`py-2 pr-4 align-top ${mono ? "font-mono text-xs" : ""} ${className}`}>
      {children}
    </td>
  );
}

export function Tag({ children, tone = "default" }: { children: ReactNode; tone?: string }) {
  const tones: Record<string, string> = {
    default: "border-ink-600 text-ink-400",
    ok: "border-ok-dim text-ok",
    warn: "border-warn-dim text-warn",
    live: "border-live-dim text-live",
  };
  return (
    <span className={`whitespace-nowrap rounded border px-1.5 py-0.5 text-[11px] ${tones[tone] ?? tones.default}`}>
      {children}
    </span>
  );
}

/**
 * The revenue chart: bars, in SVG, written here.
 *
 * A charting library is between 40 and 120 kB of JavaScript to draw thirty
 * rectangles, and this application ships one bundle to people who are mostly on
 * a phone. Hovering a bar gives the day, the money and the number of payments,
 * which is the whole of what the brief asks a chart to answer.
 */
export function Bars({
  data,
  height = 180,
}: {
  data: { label: string; total: number; parts: { value: number; tone: string; name: string }[]; note?: string }[];
  height?: number;
}) {
  const [hover, setHover] = useState<number | null>(null);
  if (data.length === 0) return <Empty title="이 기간에는 결제가 없습니다." />;
  const max = Math.max(...data.map((d) => d.total), 1);
  const gap = data.length > 40 ? 1 : 3;
  const width = 100;
  const step = width / data.length;
  const shown = hover === null ? null : data[hover];

  return (
    <div>
      <div className="flex items-start justify-between gap-4">
        <div className="text-xs text-ink-500">
          {shown ? (
            <>
              <span className="text-ink-200">{shown.label}</span>{" "}
              <span className="font-mono text-ink-100">{won(shown.total)}</span>
              {shown.note && <span className="ml-2 text-ink-500">{shown.note}</span>}
            </>
          ) : (
            "막대에 마우스를 올리면 날짜별 매출을 볼 수 있습니다"
          )}
        </div>
        <div className="font-mono text-[11px] text-ink-600">최대 {wonShort(max)}</div>
      </div>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
        className="mt-2 w-full"
        style={{ height }}
        role="img"
        aria-label="기간별 매출"
      >
        {data.map((d, i) => {
          let y = height;
          return (
            <g
              key={d.label}
              onMouseEnter={() => setHover(i)}
              onMouseLeave={() => setHover(null)}
            >
              {/* A full-height target, so a small bar is still hoverable. */}
              <rect
                x={i * step}
                y={0}
                width={step}
                height={height}
                fill={hover === i ? "rgba(255,255,255,0.04)" : "transparent"}
              />
              {d.parts.map((p) => {
                const h = (p.value / max) * (height - 4);
                y -= h;
                return (
                  <rect
                    key={p.name}
                    x={i * step + gap / 2}
                    y={y}
                    width={Math.max(step - gap, 0.5)}
                    height={h}
                    fill={p.tone}
                  />
                );
              })}
            </g>
          );
        })}
      </svg>
      <div className="mt-1 flex justify-between font-mono text-[10px] text-ink-600">
        <span>{data[0]?.label}</span>
        <span>{data[data.length - 1]?.label}</span>
      </div>
    </div>
  );
}

/** Ask before anything that cannot be taken back. */
export function Confirm({
  title,
  detail,
  confirmLabel,
  busy,
  onConfirm,
  onCancel,
  note,
  onNote,
}: {
  title: string;
  detail: string;
  confirmLabel: string;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
  note?: string;
  onNote?: (v: string) => void;
}) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4">
      <div className="w-full max-w-md rounded-lg border border-ink-700 bg-ink-900 p-5">
        <h3 className="text-sm font-semibold text-ink-100">{title}</h3>
        <p className="mt-2 text-sm text-ink-400">{detail}</p>
        {onNote && (
          <label className="mt-4 block">
            <span className="text-[11px] uppercase tracking-wider text-ink-500">사유 (감사 로그에 남습니다)</span>
            <input
              value={note ?? ""}
              onChange={(e) => onNote(e.target.value)}
              className="mt-1 w-full rounded-md border border-ink-600 bg-ink-950 px-3 py-2 text-sm text-ink-100"
              placeholder="예: 고객 요청"
            />
          </label>
        )}
        <div className="mt-5 flex justify-end gap-2">
          <button
            onClick={onCancel}
            disabled={busy}
            className="rounded-md border border-ink-600 px-3 py-1.5 text-sm text-ink-300 hover:bg-ink-800"
          >
            취소
          </button>
          <button
            onClick={onConfirm}
            disabled={busy}
            className="rounded-md bg-live px-3 py-1.5 text-sm text-white hover:brightness-110 disabled:opacity-50"
          >
            {busy ? "처리 중…" : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
