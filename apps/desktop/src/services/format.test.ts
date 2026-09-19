import { describe, expect, it } from 'vitest'
import {
  EVERYDAY, WEEKDAYS, crossesMidnight, describeDays, formatBytes, formatDuration,
  formatDurationKo, formatMbps, hasDay, isBroadcastReady, mediaStatusLabel,
  streamStateLabel, toggleDay, windowDurationSecs,
} from './format'

describe('duration formatting', () => {
  it('renders HH:MM:SS', () => {
    expect(formatDuration(0)).toBe('00:00:00')
    expect(formatDuration(3661)).toBe('01:01:01')
    expect(formatDuration(15678)).toBe('04:21:18')
  })

  it('clamps negatives rather than showing a minus sign', () => {
    expect(formatDuration(-10)).toBe('00:00:00')
  })

  it('matches the Korean form the spec shows', () => {
    // §25: "3시간 00분 37초"
    expect(formatDurationKo(3 * 3600 + 37)).toBe('3시간 00분 37초')
    expect(formatDurationKo(62)).toBe('1분 02초')
    expect(formatDurationKo(9)).toBe('9초')
  })
})

describe('byte and bitrate formatting', () => {
  it('scales units', () => {
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(2048)).toBe('2.0 KB')
    expect(formatBytes(24_800_000_000)).toBe('23.1 GB')
  })

  it('shows a dash rather than a fake number when there is no bitrate', () => {
    expect(formatMbps(0)).toBe('—')
    expect(formatMbps(Number.NaN)).toBe('—')
    expect(formatMbps(10216)).toBe('10.2 Mbps')
  })
})

describe('day-of-week mask', () => {
  it('matches the Rust bit layout: Monday is bit 0', () => {
    expect(hasDay(WEEKDAYS, 0)).toBe(true)   // Monday
    expect(hasDay(WEEKDAYS, 4)).toBe(true)   // Friday
    expect(hasDay(WEEKDAYS, 5)).toBe(false)  // Saturday
    expect(hasDay(EVERYDAY, 6)).toBe(true)   // Sunday
  })

  it('toggles a single day without disturbing the others', () => {
    let m = 0
    m = toggleDay(m, 2)
    expect(hasDay(m, 2)).toBe(true)
    expect(hasDay(m, 1)).toBe(false)
    m = toggleDay(m, 2)
    expect(m).toBe(0)
  })

  it('describes common masks the way the UI does', () => {
    expect(describeDays(EVERYDAY)).toBe('매일')
    expect(describeDays(WEEKDAYS)).toBe('월~금')
    expect(describeDays(0)).toBe('없음')
    expect(describeDays((1 << 5) | (1 << 6))).toBe('토 일')
  })
})

describe('schedule windows', () => {
  it('detects a window that runs past midnight', () => {
    expect(crossesMidnight('20:00', '02:00')).toBe(true)
    expect(crossesMidnight('20:00', '08:00')).toBe(true)
    expect(crossesMidnight('09:00', '18:00')).toBe(false)
  })

  it('measures the window length correctly in both cases', () => {
    expect(windowDurationSecs('09:00', '18:00')).toBe(9 * 3600)
    expect(windowDurationSecs('20:00', '02:00')).toBe(6 * 3600)
    expect(windowDurationSecs('20:00', '08:00')).toBe(12 * 3600)
  })
})

describe('status labels', () => {
  it('labels every stream state in Korean', () => {
    for (const s of ['IDLE', 'PREPARING', 'CONNECTING', 'LIVE', 'RECONNECTING', 'STOPPING', 'STOPPED', 'ERROR'] as const) {
      expect(streamStateLabel(s)).toBeTruthy()
    }
    expect(streamStateLabel('LIVE')).toBe('방송 중')
  })

  it('only treats compatible and normalized media as broadcastable', () => {
    expect(isBroadcastReady('normalized')).toBe(true)
    expect(isBroadcastReady('compatible')).toBe(true)
    // §14: a file that needs no further work says so in the same words,
    // whether it was optimized or was already compliant.
    expect(mediaStatusLabel('normalized')).toBe('송출 준비 완료')
    expect(mediaStatusLabel('compatible')).toBe('송출 준비 완료')
    for (const s of ['imported', 'optimization_required', 'missing', 'failed'] as const) {
      expect(isBroadcastReady(s)).toBe(false)
      expect(mediaStatusLabel(s)).toBeTruthy()
    }
  })
})
