/**
 * Every broadcast on this account, and how many of the plan's slots are in use.
 *
 * The slot count comes from the server, not from counting rows here: the browser
 * showing "2 / 3" and the server allowing a third are two different facts, and
 * only one of them is enforceable. A start that the plan refuses arrives here as
 * the server's own message.
 */
import { useEffect, useState } from 'react'
import { Badge, Button, Card, EmptyState, Field, Input, Modal, Select, Stat } from '@/components/ui'
import { formatDurationKo } from '@/services/format'
import { useTransport } from '../TransportContext'
import { RUNTIME_LABELS, holdsASlot } from '../cloud'
import type { Broadcast, CloudMedia, Dashboard, StreamDestination } from '../cloud'

function tone(b: Broadcast): 'default' | 'ok' | 'warn' | 'live' {
  if (b.runtime_state === 'RUNNING') return 'ok'
  if (b.runtime_state === 'FAILED') return 'live'
  return holdsASlot(b) ? 'warn' : 'default'
}

export function CloudDashboard({ onChanged }: { onChanged?: () => void }) {
  const t = useTransport()
  const [dash, setDash] = useState<Dashboard | null>(null)
  const [media, setMedia] = useState<CloudMedia[]>([])
  const [destinations, setDestinations] = useState<StreamDestination[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)

  useEffect(() => {
    let live = true
    t.dashboard().then((d) => live && setDash(d)).catch(() => undefined)
    // One subscription for the whole page. It is the server that decides how
    // often a snapshot arrives.
    const stop = t.watchDashboard((d) => live && setDash(d))
    return () => {
      live = false
      stop()
    }
  }, [t])

  useEffect(() => {
    let live = true
    Promise.all([t.listMedia(), t.listDestinations()])
      .then(([m, d]) => {
        if (!live) return
        setMedia(m)
        setDestinations(d)
      })
      .catch(() => undefined)
    return () => {
      live = false
    }
  }, [t, creating])

  async function act(id: string, what: 'start' | 'stop' | 'restart' | 'delete') {
    setBusy(id)
    setError(null)
    try {
      if (what === 'start') await t.startBroadcast(id)
      if (what === 'stop') await t.stopBroadcast(id)
      if (what === 'restart') await t.restartBroadcast(id)
      if (what === 'delete') await t.deleteBroadcast(id)
      setDash(await t.dashboard())
      onChanged?.()
    } catch (e) {
      setError(e instanceof Error ? e.message : '요청을 처리할 수 없습니다.')
    } finally {
      setBusy(null)
    }
  }

  const ready = media.filter((m) => m.state === 'ready')

  return (
    <div className="space-y-4">
      <Card
        title="동시 방송"
        action={
          <Button
            variant="primary"
            size="sm"
            onClick={() => setCreating(true)}
            disabled={ready.length === 0 || destinations.length === 0}
          >
            방송 만들기
          </Button>
        }
      >
        <div className="flex items-center gap-8">
          <Stat
            label="사용 중"
            value={
              <span data-testid="slots">
                {dash?.active ?? 0} / {dash?.allowed ?? 0}
              </span>
            }
            tone={dash && dash.active >= dash.allowed ? 'warn' : 'live'}
          />
          <Stat label="요금제" value={dash?.plan_label ?? '—'} />
          <Stat label="방송" value={dash?.broadcasts.length ?? 0} />
        </div>
        {dash && dash.active >= dash.allowed && (
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

      <Card title="방송">
        {!dash || dash.broadcasts.length === 0 ? (
          <EmptyState
            title="아직 방송이 없습니다"
            hint="영상을 업로드하고 송출 대상을 추가하면 방송을 만들 수 있습니다."
          />
        ) : (
          <ul className="divide-y divide-ink-700">
            {dash.broadcasts.map((b) => (
              <li key={b.id} className="flex items-center justify-between gap-4 py-3" data-testid="broadcast-row">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-sm text-ink-100">{b.name}</span>
                    <Badge tone={tone(b)}>{RUNTIME_LABELS[b.runtime_state]}</Badge>
                    {b.restart_count > 0 && <Badge tone="warn">재시작 {b.restart_count}회</Badge>}
                  </div>
                  <div className="mt-1 text-xs text-ink-500">
                    {holdsASlot(b) ? `송출 ${formatDurationKo(b.uptime_secs)}` : '중지됨'}
                    {b.last_error ? ` · ${b.last_error}` : ''}
                  </div>
                </div>
                <div className="flex shrink-0 gap-2">
                  {b.desired_state === 'running' ? (
                    <>
                      <Button size="sm" onClick={() => act(b.id, 'restart')} disabled={busy === b.id}>
                        재시작
                      </Button>
                      <Button size="sm" variant="danger" onClick={() => act(b.id, 'stop')} disabled={busy === b.id}>
                        중지
                      </Button>
                    </>
                  ) : (
                    <>
                      <Button size="sm" variant="live" onClick={() => act(b.id, 'start')} disabled={busy === b.id}>
                        시작
                      </Button>
                      <Button size="sm" onClick={() => act(b.id, 'delete')} disabled={busy === b.id}>
                        삭제
                      </Button>
                    </>
                  )}
                </div>
              </li>
            ))}
          </ul>
        )}
      </Card>

      <NewBroadcastModal
        open={creating}
        media={ready}
        destinations={destinations}
        onClose={() => setCreating(false)}
        onCreated={async () => {
          setCreating(false)
          setDash(await t.dashboard())
        }}
      />
    </div>
  )
}

function NewBroadcastModal({
  open, media, destinations, onClose, onCreated,
}: {
  open: boolean
  media: CloudMedia[]
  destinations: StreamDestination[]
  onClose: () => void
  onCreated: () => void
}) {
  const t = useTransport()
  const [name, setName] = useState('')
  const [mediaId, setMediaId] = useState('')
  const [destinationId, setDestinationId] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    if (!open) return
    setMediaId(media[0]?.id ?? '')
    setDestinationId(destinations[0]?.id ?? '')
    setError(null)
  }, [open, media, destinations])

  async function create() {
    setBusy(true)
    setError(null)
    try {
      await t.createBroadcast({ name: name.trim() || '새 방송', media_id: mediaId, destination_id: destinationId })
      setName('')
      onCreated()
    } catch (e) {
      setError(e instanceof Error ? e.message : '방송을 만들 수 없습니다.')
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      open={open}
      title="방송 만들기"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>취소</Button>
          <Button variant="primary" onClick={create} disabled={busy || !mediaId || !destinationId}>
            만들기
          </Button>
        </>
      }
    >
      <Field label="이름">
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="새 방송" />
      </Field>
      <Field label="영상">
        <Select value={mediaId} onChange={(e) => setMediaId(e.target.value)} aria-label="영상">
          {media.map((m) => (
            <option key={m.id} value={m.id}>
              {m.filename}
            </option>
          ))}
        </Select>
      </Field>
      <Field label="송출 대상">
        <Select
          value={destinationId}
          onChange={(e) => setDestinationId(e.target.value)}
          aria-label="송출 대상"
        >
          {destinations.map((d) => (
            <option key={d.id} value={d.id}>
              {d.label}
            </option>
          ))}
        </Select>
      </Field>
      <p className="text-xs text-ink-500">
        방송은 서버에서 계속됩니다. 브라우저를 닫거나 컴퓨터를 종료해도 송출은 유지됩니다.
      </p>
      {error && (
        <p role="alert" className="pt-2 text-sm text-live">
          {error}
        </p>
      )}
    </Modal>
  )
}
