/** Small shared primitives. Dark, restrained, minimal animation (§61). */
import { X } from 'lucide-react'
import type { ReactNode } from 'react'

export function Card({
  title, action, children, className = '',
}: { title?: ReactNode; action?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={`rounded-lg border border-ink-700 bg-ink-850 ${className}`}>
      {(title || action) && (
        <header className="flex items-center justify-between border-b border-ink-700 px-4 py-3">
          <h2 className="text-xs font-semibold uppercase tracking-widest text-ink-400">{title}</h2>
          {action}
        </header>
      )}
      <div className="p-4">{children}</div>
    </section>
  )
}

type ButtonVariant = 'primary' | 'ghost' | 'danger' | 'live'

export function Button({
  variant = 'ghost', size = 'md', children, className = '', ...rest
}: {
  variant?: ButtonVariant
  size?: 'sm' | 'md' | 'lg'
} & React.ButtonHTMLAttributes<HTMLButtonElement>) {
  const variants: Record<ButtonVariant, string> = {
    primary: 'bg-ink-100 text-ink-950 hover:bg-white disabled:bg-ink-600 disabled:text-ink-400',
    ghost: 'border border-ink-600 text-ink-300 hover:bg-ink-800 hover:text-ink-100 disabled:opacity-40',
    danger: 'bg-live text-white hover:brightness-110 disabled:opacity-40',
    live: 'bg-ok text-ink-950 font-semibold hover:brightness-110 disabled:bg-ink-700 disabled:text-ink-500',
  }
  const sizes = { sm: 'px-2.5 py-1 text-xs', md: 'px-3.5 py-2 text-sm', lg: 'px-6 py-4 text-base' }
  return (
    <button
      className={`rounded-md transition-colors disabled:cursor-not-allowed ${variants[variant]} ${sizes[size]} ${className}`}
      {...rest}
    >
      {children}
    </button>
  )
}

export function Toggle({
  checked, onChange, label, hint, disabled,
}: { checked: boolean; onChange: (v: boolean) => void; label: string; hint?: string; disabled?: boolean }) {
  return (
    <label className={`flex items-start justify-between gap-4 py-2.5 ${disabled ? 'opacity-50' : 'cursor-pointer'}`}>
      <span className="min-w-0">
        <span className="block text-sm text-ink-100">{label}</span>
        {hint && <span className="mt-0.5 block text-xs text-ink-400">{hint}</span>}
      </span>
      <input
        type="checkbox"
        role="switch"
        aria-label={label}
        className="sr-only"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span
        aria-hidden
        className={`mt-0.5 h-5 w-9 shrink-0 rounded-full p-0.5 transition-colors ${checked ? 'bg-ok' : 'bg-ink-600'}`}
      >
        <span className={`block h-4 w-4 rounded-full bg-white transition-transform ${checked ? 'translate-x-4' : ''}`} />
      </span>
    </label>
  )
}

export function Field({
  label, hint, children,
}: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="py-2.5">
      <label className="mb-1.5 block text-sm text-ink-100">{label}</label>
      {children}
      {hint && <p className="mt-1 text-xs text-ink-400">{hint}</p>}
    </div>
  )
}

export function Input(props: React.InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      {...props}
      className={`w-full rounded-md border border-ink-600 bg-ink-900 px-3 py-2 text-sm text-ink-100 outline-none focus:border-ink-400 ${props.className ?? ''}`}
    />
  )
}

export function Select(props: React.SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <select
      {...props}
      className={`w-full rounded-md border border-ink-600 bg-ink-900 px-3 py-2 text-sm text-ink-100 outline-none focus:border-ink-400 ${props.className ?? ''}`}
    />
  )
}

export function Modal({
  open, title, onClose, children, footer,
}: { open: boolean; title: string; onClose: () => void; children: ReactNode; footer?: ReactNode }) {
  if (!open) return null
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6" role="dialog" aria-modal="true" aria-label={title}>
      <div className="w-full max-w-lg rounded-lg border border-ink-700 bg-ink-850 shadow-2xl">
        <header className="flex items-center justify-between border-b border-ink-700 px-5 py-3.5">
          <h2 className="text-sm font-semibold text-ink-100">{title}</h2>
          <button onClick={onClose} aria-label="닫기" className="text-ink-400 hover:text-ink-100">
            <X size={16} />
          </button>
        </header>
        <div className="px-5 py-4 text-sm text-ink-300">{children}</div>
        {footer && <footer className="flex justify-end gap-2 border-t border-ink-700 px-5 py-3">{footer}</footer>}
      </div>
    </div>
  )
}

export function Stat({ label, value, tone = 'default' }: { label: string; value: ReactNode; tone?: 'default' | 'live' | 'warn' }) {
  const tones = { default: 'text-ink-100', live: 'text-ok', warn: 'text-warn' }
  return (
    <div>
      <div className="text-[11px] uppercase tracking-wider text-ink-500">{label}</div>
      <div className={`mt-1 font-mono text-lg ${tones[tone]}`}>{value}</div>
    </div>
  )
}

export function Badge({ children, tone = 'default' }: { children: ReactNode; tone?: 'default' | 'ok' | 'warn' | 'live' }) {
  const tones = {
    default: 'border-ink-600 text-ink-400',
    ok: 'border-ok-dim text-ok',
    warn: 'border-warn-dim text-warn',
    live: 'border-live-dim text-live',
  }
  return (
    <span className={`rounded border px-1.5 py-0.5 text-[11px] ${tones[tone]}`}>{children}</span>
  )
}

export function ProgressBar({ percent, label }: { percent: number; label?: string }) {
  const p = Math.max(0, Math.min(100, percent))
  return (
    <div>
      {label && (
        <div className="mb-1 flex justify-between text-xs text-ink-400">
          <span>{label}</span>
          <span className="font-mono">{p.toFixed(0)}%</span>
        </div>
      )}
      <div className="h-1.5 overflow-hidden rounded-full bg-ink-700">
        <div
          role="progressbar"
          aria-valuenow={Math.round(p)}
          aria-valuemin={0}
          aria-valuemax={100}
          className="h-full bg-ok transition-[width] duration-300"
          style={{ width: `${p}%` }}
        />
      </div>
    </div>
  )
}

export function EmptyState({ icon, title, hint, action }: { icon?: ReactNode; title: string; hint?: string; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-3 py-12 text-center">
      {icon && <div className="text-ink-600">{icon}</div>}
      <p className="text-sm text-ink-300">{title}</p>
      {hint && <p className="max-w-sm text-xs text-ink-500">{hint}</p>}
      {action}
    </div>
  )
}
