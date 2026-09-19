import { useEffect, useState } from 'react'
import { CalendarPlus, Trash2 } from 'lucide-react'
import { useAppStore } from '@/stores/useAppStore'
import { api } from '@/services/ipc'
import { DAY_LABELS, EVERYDAY, WEEKDAYS, crossesMidnight, hasDay, toggleDay } from '@/services/format'
import { Badge, Button, Card, EmptyState, Field, Input, Select, Toggle } from '@/components/ui'

/** Schedule page (§19, §27). */
export function SchedulePage() {
  const { schedules, playlists, refreshSchedules, refreshPlaylists, reportError, toast } = useAppStore()

  const [playlistId, setPlaylistId] = useState<number | ''>('')
  const [days, setDays] = useState<number>(EVERYDAY)
  const [start, setStart] = useState('20:00')
  const [end, setEnd] = useState('08:00')

  useEffect(() => {
    void refreshSchedules()
    void refreshPlaylists()
  }, [refreshSchedules, refreshPlaylists])

  useEffect(() => {
    if (playlistId === '' && playlists[0]) setPlaylistId(playlists[0].id)
  }, [playlists, playlistId])

  const overnight = crossesMidnight(start, end)

  async function create() {
    if (playlistId === '') return toast({ kind: 'error', message: '플레이리스트를 선택해주세요.' })
    if (days === 0) return toast({ kind: 'error', message: '반복할 요일을 하나 이상 선택해주세요.' })
    try {
      await api.createSchedule(Number(playlistId), days, start, end)
      toast({ kind: 'success', message: '예약을 추가했습니다.' })
      await refreshSchedules()
    } catch (e) {
      reportError(e)
    }
  }

  return (
    <div className="space-y-4">
      <h1 className="text-lg font-semibold text-ink-100">방송 예약</h1>

      <Card title="새 예약">
        <div className="grid gap-4 md:grid-cols-2">
          <Field label="플레이리스트">
            <Select value={playlistId} onChange={(e) => setPlaylistId(Number(e.target.value))} aria-label="플레이리스트">
              {playlists.length === 0 && <option value="">먼저 플레이리스트를 만들어주세요</option>}
              {playlists.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
            </Select>
          </Field>
          <div className="grid grid-cols-2 gap-3">
            <Field label="시작 시간">
              <Input type="time" value={start} onChange={(e) => setStart(e.target.value)} aria-label="시작 시간" />
            </Field>
            <Field label="종료 시간">
              <Input type="time" value={end} onChange={(e) => setEnd(e.target.value)} aria-label="종료 시간" />
            </Field>
          </div>
        </div>

        <Field label="반복" hint={overnight ? '자정을 넘겨 다음 날까지 방송합니다.' : undefined}>
          <div className="flex flex-wrap items-center gap-1.5">
            {DAY_LABELS.map((d, i) => (
              <button
                key={d}
                type="button"
                aria-pressed={hasDay(days, i)}
                onClick={() => setDays((m) => toggleDay(m, i))}
                className={`h-9 w-9 rounded-md border text-sm transition-colors ${
                  hasDay(days, i)
                    ? 'border-ink-400 bg-ink-700 text-ink-100'
                    : 'border-ink-700 text-ink-500 hover:text-ink-300'
                }`}
              >
                {d}
              </button>
            ))}
            <span className="mx-2 h-5 w-px bg-ink-700" />
            <Button size="sm" onClick={() => setDays(EVERYDAY)}>매일</Button>
            <Button size="sm" onClick={() => setDays(WEEKDAYS)}>월~금</Button>
          </div>
        </Field>

        {overnight && (
          <p className="mb-3 text-xs text-warn">
            {start} → 다음 날 {end} 까지 방송합니다.
          </p>
        )}

        <Button variant="primary" onClick={() => void create()}>
          <span className="inline-flex items-center gap-2"><CalendarPlus size={15} /> 예약 추가</span>
        </Button>
      </Card>

      <Card title="예약 목록">
        {schedules.length === 0 ? (
          <EmptyState title="예약된 방송이 없습니다" hint="예약을 추가하면 지정한 시간에 자동으로 방송이 시작됩니다." />
        ) : (
          <ul className="divide-y divide-ink-800">
            {schedules.map((s) => (
              <li key={s.id} className="flex flex-wrap items-center justify-between gap-3 py-3">
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-sm text-ink-100">{s.start_time} → {s.end_time}</span>
                    {s.crosses_midnight && <Badge tone="warn">자정 넘김</Badge>}
                    <Badge>{s.days_label}</Badge>
                    <span className="text-xs text-ink-400">{s.playlist_name ?? `#${s.playlist_id}`}</span>
                  </div>
                  <div className="mt-1 text-[11px] text-ink-500">
                    {s.enabled && s.next_start
                      ? `다음 방송 ${s.next_start} · ${s.window_duration_label} 방송`
                      : '사용 안 함'}
                  </div>
                </div>
                <div className="flex items-center gap-3">
                  <div className="w-28">
                    <Toggle
                      label=""
                      checked={s.enabled}
                      onChange={async (v) => {
                        try {
                          await api.updateSchedule(s.id, s.playlist_id, s.days_of_week, s.start_time, s.end_time, v)
                          await refreshSchedules()
                        } catch (e) { reportError(e) }
                      }}
                    />
                  </div>
                  <button
                    aria-label="예약 삭제"
                    onClick={async () => {
                      try {
                        await api.deleteSchedule(s.id)
                        await refreshSchedules()
                      } catch (e) { reportError(e) }
                    }}
                    className="rounded p-1.5 text-ink-600 hover:text-live"
                  >
                    <Trash2 size={15} />
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </Card>
    </div>
  )
}
