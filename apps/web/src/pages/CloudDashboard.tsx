/**
 * Every broadcast on this account, and how many of the plan's slots are in use.
 *
 * The slot count comes from the server, not from counting rows here: the browser
 * showing "2 / 3" and the server allowing a third are two different facts, and
 * only one of them is enforceable. A start that the plan refuses arrives here as
 * the server's own message.
 */
import { useEffect, useState } from 'react'
import { Badge, Button, Card, EmptyState, ProgressBar, Stat } from '@/components/ui'
import { formatBytes, formatDurationKo } from '@/services/format'
import { useTransport } from '../TransportContext'
import { RUNTIME_LABELS, holdsASlot, itemPercent, playlistLabel, scheduleLabel } from '../cloud'
import { BroadcastForm } from './BroadcastForm'
import type { Broadcast, BroadcastDetail, Dashboard, StreamDestination } from '../cloud'

/** The host a stream is going to, which is the first thing to check when a
 * channel shows nothing. */
function hostOf(url: string): string {
  return url.split('://')[1]?.split('/')[0] ?? url
}

function tone(b: Broadcast): 'default' | 'ok' | 'warn' | 'live' {
  if (b.runtime_state === 'RUNNING') return 'ok'
  if (b.runtime_state === 'FAILED') return 'live'
  return holdsASlot(b) ? 'warn' : 'default'
}

export function CloudDashboard({ onChanged }: { onChanged?: () => void }) {
  const t = useTransport()
  const [dash, setDash] = useState<Dashboard | null>(null)
  const [destinations, setDestinations] = useState<StreamDestination[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  /** `null` — the list. `'new'` — the create form. Otherwise the one being edited. */
  const [editing, setEditing] = useState<'new' | BroadcastDetail | null>(null)

  async function refresh() {
    try {
      setDash(await t.dashboard())
    } catch {
      /* the next snapshot will do */
    }
  }

  useEffect(() => {
    let live = true
    t.dashboard().then((d) => live && setDash(d)).catch(() => undefined)
    t.listDestinations().then((d) => live && setDestinations(d)).catch(() => undefined)
    // One subscription for the whole page. It is the server that decides how
    // often a snapshot arrives, and the card redraws from it without a reload.
    const stop = t.watchDashboard((d) => live && setDash(d))
    return () => {
      live = false
      stop()
    }
  }, [t, editing])

  async function act(id: string, what: 'start' | 'stop' | 'restart' | 'delete') {
    setBusy(id)
    setError(null)
    try {
      if (what === 'start') await t.startBroadcast(id)
      if (what === 'stop') await t.stopBroadcast(id)
      if (what === 'restart') await t.restartBroadcast(id)
      if (what === 'delete') await t.deleteBroadcast(id)
      await refresh()
      onChanged?.()
    } catch (e) {
      setError(e instanceof Error ? e.message : '요청을 처리할 수 없습니다.')
    } finally {
      setBusy(null)
    }
  }

  if (editing) {
    return (
      <BroadcastForm
        {...(editing === 'new' ? {} : { editing })}
        onDone={() => {
          setEditing(null)
          refresh()
        }}
        onCancel={() => setEditing(null)}
      />
    )
  }

  const full = !!dash && dash.active >= dash.allowed

  return (
    <div className="space-y-4">
      <Card
        title="동시 방송"
        action={
          <Button variant="primary" size="sm" onClick={() => setEditing('new')}>
            방송 만들기
          </Button>
        }
      >
        <div className="flex flex-wrap items-center gap-8">
          <Stat
            label="사용 중"
            value={
              <span data-testid="slots">
                {dash?.active ?? 0} / {dash?.allowed ?? 0}
              </span>
            }
            tone={full ? 'warn' : 'live'}
          />
          <Stat label="요금제" value={dash?.plan_label ?? '—'} />
          <Stat label="방송" value={dash?.broadcasts.length ?? 0} />
        </div>
        {full && (
          <p className="mt-3 text-xs text-warn">
            요금제의 동시 방송 수를 모두 사용하고 있습니다. 하나를 중지하면 다른 방송을 시작할 수 있습니다.
          </p>
        )}
      </Card>

      {error && (
        <p role="alert" className="rounded-md border border-live-dim bg-ink-850 px-4 py-3 text-sm text-live">
          {error}
        </p>
      )}

      {!dash || dash.broadcasts.length === 0 ? (
        <Card title="방송">
          <EmptyState
            title="아직 방송이 없습니다"
            hint="영상을 업로드하고 송출 대상을 추가하면 방송을 만들 수 있습니다."
          />
        </Card>
      ) : (
        dash.broadcasts.map((b) => (
          <BroadcastCard
            key={b.id}
            b={b}
            destination={destinations.find((d) => d.id === b.destination_id)}
            busy={busy === b.id}
            onAct={(what) => act(b.id, what)}
            onEdit={async () => {
              try {
                setEditing(await t.getBroadcast(b.id))
              } catch (e) {
                setError(e instanceof Error ? e.message : '방송을 열 수 없습니다.')
              }
            }}
          />
        ))
      )}
    </div>
  )
}

/** One broadcast, as an operator needs to see it. §10. */
function BroadcastCard({
  b, destination, busy, onAct, onEdit,
}: {
  b: Broadcast
  destination?: StreamDestination
  busy: boolean
  onAct: (what: 'start' | 'stop' | 'restart' | 'delete') => void
  onEdit: () => void
}) {
  const live = holdsASlot(b)
  const scheduled = !live && b.schedule.enabled
  const status = scheduled ? '예약됨' : RUNTIME_LABELS[b.runtime_state]
  const when = scheduleLabel(b.schedule)

  return (
    <Card
      title={
        <span className="flex items-center gap-2">
          <span className="text-sm font-semibold normal-case tracking-normal text-ink-100">{b.name}</span>
          <Badge tone={tone(b)}>{status}</Badge>
          {b.restart_count > 0 && <Badge tone="warn">재시작 {b.restart_count}회</Badge>}
        </span>
      }
      action={
        <div className="flex shrink-0 gap-2">
          {b.desired_state === 'running' ? (
            <>
              <Button size="sm" onClick={() => onAct('restart')} disabled={busy}>
                재시작
              </Button>
              <Button size="sm" variant="danger" onClick={() => onAct('stop')} disabled={busy}>
                중지
              </Button>
            </>
          ) : (
            <>
              <Button size="sm" variant="live" onClick={() => onAct('start')} disabled={busy}>
                시작
              </Button>
              <Button size="sm" onClick={onEdit} disabled={busy}>
                수정
              </Button>
              <Button size="sm" onClick={() => onAct('delete')} disabled={busy}>
                삭제
              </Button>
            </>
          )}
        </div>
      }
    >
      <div data-testid="broadcast-row" data-state={b.runtime_state}>
        {live && (
          <div className="mb-4">
            <p className="text-[11px] uppercase tracking-wider text-ink-500">지금 재생 중</p>
            <p className="truncate text-sm text-ink-100" data-testid="now-playing">
              {b.current_item ?? '—'}
            </p>
            <div className="mt-2">
              <ProgressBar
                percent={itemPercent(b)}
                label={`${formatDurationKo(b.current_position_secs)} / ${formatDurationKo(b.current_duration_secs)}`}
              />
            </div>
            {b.next_item && (
              <p className="mt-2 text-xs text-ink-500">
                다음 · <span data-testid="next-up">{b.next_item}</span>
              </p>
            )}
          </div>
        )}

        <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
          <Stat label="플레이리스트" value={<span data-testid="playlist-progress">{playlistLabel(b)}</span>} />
          <Stat label="송출 시간" value={live ? formatDurationKo(b.uptime_secs) : '—'} tone={live ? 'live' : 'default'} />
          <Stat
            label="평균 비트레이트"
            value={b.uptime_secs > 0 ? `${((b.bytes_sent * 8) / b.uptime_secs / 1_000_000).toFixed(2)} Mbps` : '—'}
          />
          <Stat label="전송량" value={b.bytes_sent > 0 ? formatBytes(b.bytes_sent) : '—'} />
        </div>

        {live && (
          <p className="mt-3 rounded-md border border-ink-700 bg-ink-900 px-3 py-2 text-xs text-ink-400">
            <span className="text-ink-100">RTMPS 전송 중</span>
            {destination ? ` → ${hostOf(destination.rtmps_url)}` : ''}
            {destination?.kind !== 'youtube_account' && (
              <>
                {' · '}스트림 키 방식이므로 채널에 공개하려면 YouTube Studio에서 수신을 확인하고
                <span className="text-ink-100"> 실시간 시작</span>을 눌러야 합니다.
              </>
            )}
          </p>
        )}

        <div className="mt-4 flex flex-wrap gap-x-6 gap-y-1 border-t border-ink-800 pt-3 text-xs text-ink-500">
          <span>대상 · {destination?.label ?? b.destination_id.slice(0, 8)}</span>
          {when && <span data-testid="schedule-label">예약 · {when}</span>}
          {b.ffmpeg_pid && <span>FFmpeg · {b.ffmpeg_pid}</span>}
          <span>{b.loop_forever ? '전체 반복' : '한 바퀴만'}</span>
          {b.last_error && <span className="text-live">{b.last_error}</span>}
        </div>
      </div>
    </Card>
  )
}
