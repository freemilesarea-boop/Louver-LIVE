/** Presentation helpers. Kept pure so they are covered by unit tests. */

/** `HH:MM:SS`, clamped at zero. */
export function formatDuration(totalSeconds: number): string {
  const s = Math.max(0, Math.floor(totalSeconds))
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${pad(Math.floor(s / 3600))}:${pad(Math.floor((s % 3600) / 60))}:${pad(s % 60)}`
}

/** Korean form used for playlist totals, e.g. `3시간 00분 37초` (§25). */
export function formatDurationKo(totalSeconds: number): string {
  const s = Math.max(0, Math.floor(totalSeconds))
  const [h, m, sec] = [Math.floor(s / 3600), Math.floor((s % 3600) / 60), s % 60]
  const pad = (n: number) => String(n).padStart(2, '0')
  if (h > 0) return `${h}시간 ${pad(m)}분 ${pad(sec)}초`
  if (m > 0) return `${m}분 ${pad(sec)}초`
  return `${sec}초`
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = bytes / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i += 1
  }
  return `${v.toFixed(1)} ${units[i]}`
}

export function formatMbps(kbps: number): string {
  if (!Number.isFinite(kbps) || kbps <= 0) return '—'
  return `${(kbps / 1000).toFixed(1)} Mbps`
}

export function formatResolution(w: number, h: number): string {
  return w && h ? `${w}×${h}` : '—'
}

// --- day-of-week mask (§19, §27) -------------------------------------------

export const DAY_LABELS = ['월', '화', '수', '목', '금', '토', '일'] as const
export const EVERYDAY = 0b0111_1111
export const WEEKDAYS = 0b0001_1111

export function toggleDay(mask: number, dayIndex: number): number {
  return mask ^ (1 << dayIndex)
}

export function hasDay(mask: number, dayIndex: number): boolean {
  return (mask & (1 << dayIndex)) !== 0
}

export function describeDays(mask: number): string {
  if ((mask & EVERYDAY) === EVERYDAY) return '매일'
  if ((mask & EVERYDAY) === WEEKDAYS) return '월~금'
  const days = DAY_LABELS.filter((_, i) => hasDay(mask, i))
  return days.length ? days.join(' ') : '없음'
}

/** True when the window runs past midnight (§19). */
export function crossesMidnight(start: string, end: string): boolean {
  return end <= start
}

/** Length of a window in seconds, handling the midnight case. */
export function windowDurationSecs(start: string, end: string): number {
  const toSecs = (t: string) => {
    const [h = '0', m = '0'] = t.split(':')
    return Number(h) * 3600 + Number(m) * 60
  }
  const [s, e] = [toSecs(start), toSecs(end)]
  return crossesMidnight(start, end) ? 86400 - s + e : e - s
}

// --- status presentation ----------------------------------------------------

import type { MediaStatus, StreamState } from '@/types'

export function streamStateLabel(s: StreamState): string {
  const map: Record<StreamState, string> = {
    IDLE: '대기 중',
    PREPARING: '준비 중',
    CONNECTING: '연결 중',
    LIVE: '방송 중',
    RECONNECTING: '재연결 중',
    STOPPING: '종료 중',
    STOPPED: '종료됨',
    ERROR: '오류',
  }
  return map[s]
}

export function mediaStatusLabel(s: MediaStatus): string {
  const map: Record<MediaStatus, string> = {
    imported: '분석됨',
    compatible: '송출 준비 완료',
    optimization_required: '최적화 필요',
    normalized: '송출 준비 완료',
    missing: '파일 없음',
    failed: '실패',
  }
  return map[s]
}

export function isBroadcastReady(s: MediaStatus): boolean {
  return s === 'compatible' || s === 'normalized'
}
