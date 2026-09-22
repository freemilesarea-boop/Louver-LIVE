import { describe, expect, it } from 'vitest'
import {
  EVERYDAY, WEEKDAYS, crossesMidnight, describeDays, formatBytes, formatDuration, formatEta,
  formatDurationKo, formatMbps, hasDay, isBroadcastReady, mediaStatusLabel,
  streamStateLabel, toggleDay, windowDurationSecs, encoderLabel,
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

  it('treats only a prepared file as broadcastable', () => {
    expect(isBroadcastReady('normalized')).toBe(true)
    expect(mediaStatusLabel('normalized')).toBe('송출 준비 완료')

    // `compatible` means the source matches the profile, which buys a packet
    // copy rather than an encode — not a way past preparation. Streaming one
    // straight from the user's path faults at every concat join; the Rust
    // test `a_source_file_used_untouched_breaks_the_loop` is the proof.
    expect(isBroadcastReady('compatible')).toBe(false)

    for (const s of ['imported', 'compatible', 'optimization_required', 'missing', 'failed'] as const) {
      expect(isBroadcastReady(s)).toBe(false)
      expect(mediaStatusLabel(s)).toBeTruthy()
    }
  })

  // §1: the word never reaches a user-facing string.
  it('never calls preparation "최적화"', () => {
    const statuses = ['imported', 'compatible', 'optimization_required', 'normalized', 'missing', 'failed'] as const
    for (const s of statuses) {
      expect(mediaStatusLabel(s)).not.toContain('최적화')
    }
  })

  // §7: the engine is reported in words, and never asked for.
  it('names the conversion engine without making the user read an encoder name', () => {
    expect(encoderLabel('h264_nvenc')).toBe('NVIDIA GPU')
    expect(encoderLabel('h264_qsv')).toBe('Intel Quick Sync')
    expect(encoderLabel('h264_amf')).toBe('AMD GPU')
    expect(encoderLabel('libx264')).toBe('CPU')
    expect(encoderLabel('something_new')).toBe('CPU')
  })
})

describe('formatEta', () => {
  it('says nothing it cannot know', () => {
    expect(formatEta(-1)).toBe('계산 중')
    expect(formatEta(Number.NaN)).toBe('계산 중')
    expect(formatEta(Number.POSITIVE_INFINITY)).toBe('계산 중')
  })

  it('counts seconds only when seconds are what matters', () => {
    expect(formatEta(3)).toBe('10초 미만')
    expect(formatEta(22)).toBe('25초')
    expect(formatEta(59)).toBe('60초')
  })

  it('rounds up to whole minutes, so it never promises too little', () => {
    expect(formatEta(61)).toBe('2분')
    expect(formatEta(240)).toBe('4분')
  })

  it('reads as hours once it is one', () => {
    expect(formatEta(3600)).toBe('1시간 0분')
    expect(formatEta(5400)).toBe('1시간 30분')
  })
})
