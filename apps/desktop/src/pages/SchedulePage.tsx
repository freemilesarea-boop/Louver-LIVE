import { useCallback, useEffect, useState } from 'react'
import { CalendarPlus, Trash2 } from 'lucide-react'
import { useAppStore } from '@/stores/useAppStore'
import { api } from '@/services/ipc'
import { DAY_LABELS, EVERYDAY, WEEKDAYS, crossesMidnight, hasDay, toggleDay } from '@/services/format'
import { Badge, Button, Card, EmptyState, Field, Input, Select, Toggle } from '@/components/ui'
import type { SchedulerStatusView } from '@/types'

/** How the scheduler's state reads on screen. */
const STATE_LABEL: Record<SchedulerStatusView['state'], string> = {
  STOPPED: '꺼짐',
  ARMING: '확인 중',
  WAITING: '예약 대기 중',
  STARTING: '방송 시작 중',
  LIVE: '예약 방송 중',
  STOPPING: '종료 중',
  ERROR: '오류',
}

function countdown(total: number): string {
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${pad(h)}:${pad(m)}:${pad(s)}`
}

/** Schedule page (§19, §27). */
export function SchedulePage() {
  const { schedules, playlists, settings, refreshSchedules, refreshPlaylists, reportError, toast } = useAppStore()

  const [playlistId, setPlaylistId] = useState<number | ''>('')
  const [days, setDays] = useState<number>(EVERYDAY)
  const [start, setStart] = useState('20:00')
  const [end, setEnd] = useState('08:00')
  const [sched, setSched] = useState<SchedulerStatusView | null>(null)
  const [busy, setBusy] = useState(false)

  const refreshScheduler = useCallback(
    () => api.schedulerStatus().then(setSched).catch(() => {}),
    [],
  )

  useEffect(() => {
    void refreshSchedules()
    void refreshPlaylists()
    void refreshScheduler()
  }, [refreshSchedules, refreshPlaylists, refreshScheduler])

  // The countdown has to move, or "예약 대기 중" is just another static label
  // that proves nothing.
  useEffect(() => {
    const t = setInterval(() => void refreshScheduler(), 1000)
    return () => clearInterval(t)
  }, [refreshScheduler])

  async function arm() {
    setBusy(true)
    try {
      setSched(await api.schedulerArm())
      toast({ kind: 'success', message: '예약 방송을 시작했습니다. 예약 시간이 되면 자동으로 방송합니다.' })
    } catch (e) { reportError(e) } finally { setBusy(false) }
  }

  async function disarm() {
    setBusy(true)
    try {
      setSched(await api.schedulerDisarm())
      toast({ kind: 'info', message: '예약 방송을 중지했습니다. 예약은 그대로 저장되어 있습니다.' })
    } catch (e) { reportError(e) } finally { setBusy(false) }
  }

  useEffect(() => {
    if (playlistId === '' && playlists[0]) setPlaylistId(playlists[0].id)
  }, [playlists, playlistId])

  const overnight = crossesMidnight(start, end)

  /** Fill the form with a window that begins in a few minutes, today. */
  function applyOffsets(startInMin: number, endInMin: number) {
    const at = (min: number) => {
      const d = new Date(Date.now() + min * 60_000)
      return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
    }
    setStart(at(startInMin))
    setEnd(at(endInMin))
    setDays(EVERYDAY)
    toast({ kind: 'info', message: `${at(startInMin)} 시작 · ${at(endInMin)} 종료로 채웠습니다. 예약 추가를 누르세요.` })
  }

  async function create() {
    if (playlistId === '') return toast({ kind: 'error', message: '플레이리스트를 선택해주세요.' })
    if (days === 0) return toast({ kind: 'error', message: '반복할 요일을 하나 이상 선택해주세요.' })
    try {
      await api.createSchedule(Number(playlistId), days, start, end)
      // §8: "저장했습니다" alone is what left the user wondering whether
      // anything would actually happen. Say which of the two it is.
      const after = await api.schedulerStatus().catch(() => null)
      setSched(after)
      toast(after?.armed
        ? { kind: 'success', message: '예약이 저장되었으며 자동 방송에 반영되었습니다.' }
        : {
            kind: 'info',
            message: '예약이 저장되었습니다. 자동 방송을 사용하려면 아래 [예약 방송 시작]을 눌러주세요.',
          })
      await refreshSchedules()
    } catch (e) {
      reportError(e)
    }
  }

  return (
    <div className="space-y-4">
      <h1 className="text-lg font-semibold text-ink-100">방송 예약</h1>

      {/* The one thing the old screen could not answer: is this computer
          actually watching the clock? A saved rule and a running scheduler
          looked identical, so a user who had only done the first waited all
          night for a broadcast nothing was going to start. */}
      <Card>
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0" data-testid="scheduler-state">
            <div className="flex items-center gap-2">
              <span
                className={`h-2 w-2 shrink-0 rounded-full ${
                  sched?.state === 'LIVE' ? 'bg-live'
                    : sched?.armed ? 'bg-ok' : 'bg-ink-600'
                }`}
              />
              <span className="text-xl text-ink-100">
                {sched ? STATE_LABEL[sched.state] : '—'}
              </span>
            </div>
            {sched?.armed ? (
              sched.active_start ? (
                <p className="mt-1 text-xs text-ink-400">
                  {sched.active_start.slice(11)} → {sched.active_end?.slice(11)} 방송 중입니다.
                </p>
              ) : sched.next_start ? (
                <div className="mt-1 text-xs text-ink-400">
                  <div>다음 방송 {sched.next_start} → {sched.next_end?.slice(11)}{sched.next_playlist ? ` · ${sched.next_playlist}` : ''}</div>
                  {sched.seconds_until_start != null && (
                    <div className="mt-0.5 font-mono text-sm text-ok" data-testid="scheduler-countdown">
                      {countdown(sched.seconds_until_start)} 후 자동 시작
                    </div>
                  )}
                </div>
              ) : (
                <p className="mt-1 text-xs text-warn">사용 중인 예약이 없어 기다릴 방송이 없습니다.</p>
              )
            ) : (
              <p className="mt-1 text-xs text-ink-500">
                예약은 저장되어 있지만 자동 방송은 꺼져 있습니다.
                시작을 눌러야 이 컴퓨터가 예약 시간을 감시합니다.
              </p>
            )}
            {sched?.last_error && (
              <p className="mt-1 text-xs text-live">{sched.last_error.message}</p>
            )}
          </div>
          <div className="shrink-0">
            {sched?.armed ? (
              <Button size="lg" variant="danger" onClick={() => void disarm()} disabled={busy}>
                예약 방송 중지
              </Button>
            ) : (
              <Button size="lg" variant="live" onClick={() => void arm()} disabled={busy}>
                예약 방송 시작
              </Button>
            )}
          </div>
        </div>
      </Card>

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

        {/* Developer mode only (§10). Waiting until 20:00 to find out whether the
            scheduler fires is no way to test it, so this fills the form with a
            window that starts in a minute. It only sets the fields — the
            schedule it creates goes through exactly the same path as any
            other. */}
        {settings?.developer_mode && (
          <div className="mt-4 flex flex-wrap items-center gap-2 border-t border-ink-800 pt-3">
            <span className="text-[11px] uppercase tracking-wider text-ink-500">테스트 프리셋</span>
            <Button size="sm" onClick={() => applyOffsets(1, 3)}>1분 후 시작 · 3분 후 종료</Button>
            <Button size="sm" onClick={() => applyOffsets(2, 10)}>2분 후 시작 · 10분 후 종료</Button>
          </div>
        )}
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
                    {/* An open window is not a "next" broadcast. Showing only
                        next_start meant a 17:14→17:40 schedule read at 17:17
                        announced *tomorrow*, which looked like a skip. */}
                    {!s.enabled ? '사용 안 함'
                      : s.active_now ? (
                        sched?.armed ? (
                          <span className="text-ok">
                            지금 방송 시간입니다 · {s.active_until}에 종료
                          </span>
                        ) : (
                          // The window is open, but nothing is watching it.
                          // Saying "지금 방송 시간입니다" here would be the same
                          // false promise the global switch exists to end.
                          <span className="text-warn">
                            지금이 예약 시간이지만 자동 방송이 꺼져 있습니다
                          </span>
                        )
                      ) : s.next_start
                        ? `다음 방송 ${s.next_start} · ${s.window_duration_label} 방송`
                        : '예정된 방송이 없습니다'}
                  </div>
                  {s.playlist_missing ? (
                    <div className="mt-1 text-[11px] text-live">
                      예약에 연결된 플레이리스트를 찾을 수 없습니다.
                    </div>
                  ) : s.enabled && s.playlist_ready_count === 0 ? (
                    <div className="mt-1 text-[11px] text-warn">
                      예약된 플레이리스트에 방송 가능한 영상이 없습니다.
                    </div>
                  ) : null}
                </div>
                <div className="flex items-center gap-3">
                  <div className="w-28">
                    <Toggle
                      label="이 예약 사용"
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
