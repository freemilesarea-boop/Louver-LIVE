import type { StreamState } from '@/types'
import { streamStateLabel } from '@/services/format'

/**
 * The broadcast state indicator (§24).
 *
 * §61 asks for a restrained palette where LIVE is the only strong colour, so
 * everything else stays neutral and only the live dot pulses.
 */
export function StatusPill({ state, dryRun }: { state: StreamState; dryRun?: boolean }) {
  const live = state === 'LIVE'
  const busy = state === 'CONNECTING' || state === 'PREPARING' || state === 'RECONNECTING'
  const bad = state === 'ERROR'

  const dot = live ? (dryRun ? 'bg-warn' : 'bg-ok') : busy ? 'bg-warn' : bad ? 'bg-live' : 'bg-ink-500'
  const text = live ? (dryRun ? 'text-warn' : 'text-ok') : busy ? 'text-warn' : bad ? 'text-live' : 'text-ink-400'

  return (
    <span className="inline-flex items-center gap-2" data-testid="status-pill" data-state={state}>
      <span className={`relative flex h-2 w-2`}>
        {live && <span className={`absolute inline-flex h-full w-full animate-ping rounded-full opacity-60 ${dot}`} />}
        <span className={`relative inline-flex h-2 w-2 rounded-full ${dot}`} />
      </span>
      <span className={`text-xs font-semibold uppercase tracking-widest ${text}`}>
        {live ? (dryRun ? 'TEST' : 'LIVE') : state === 'IDLE' || state === 'STOPPED' ? 'OFFLINE' : streamStateLabel(state)}
      </span>
    </span>
  )
}
