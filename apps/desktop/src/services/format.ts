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
    imported: '확인 중…',
    compatible: '방송 준비 필요',
    optimization_required: '방송 준비 필요',
    normalized: '송출 준비 완료',
    missing: '파일 없음',
    failed: '실패',
  }
  return map[s]
}

/**
 * Only a prepared file is broadcastable.
 *
 * `compatible` means the source matches the profile, which is not the same
 * thing: see the note on `MediaStatus::Compatible` in the Rust models. It
 * buys a packet copy instead of an encode, not a way past preparation.
 */
export function isBroadcastReady(s: MediaStatus): boolean {
  return s === 'normalized'
}

/**
 * How long is left, said the way a person would say it.
 *
 * Deliberately coarse: an estimate that reads "약 4분" and turns out to be
 * three is forgivable, while "4분 12초" ticking down unevenly is not. Under a
 * minute it counts seconds, because that is when the number is about to
 * matter.
 */
export function formatEta(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '계산 중'
  const s = Math.ceil(seconds)
  if (s < 10) return '10초 미만'
  if (s < 60) return `${Math.ceil(s / 5) * 5}초`
  const m = Math.ceil(s / 60)
  if (m < 60) return `${m}분`
  const h = Math.floor(m / 60)
  return `${h}시간 ${m % 60}분`
}

/**
 * The conversion engine in the words §7 asks for.
 *
 * The user never picks one — the backend probes for a working encoder and
 * takes the first that answers, NVENC then Quick Sync then AMF then the CPU.
 * This only reports which that turned out to be.
 */
export function encoderLabel(encoder: string): string {
  const map: Record<string, string> = {
    h264_nvenc: 'NVIDIA GPU',
    h264_qsv: 'Intel Quick Sync',
    h264_amf: 'AMD GPU',
    h264_videotoolbox: 'Apple 하드웨어 가속',
  }
  return map[encoder] ?? 'CPU'
}
