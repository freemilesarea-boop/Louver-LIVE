/**
 * Making and editing a broadcast: information, playlist, destination, sending,
 * schedule, and one last look.
 *
 * Sections on one page rather than a wizard of steps, because every answer is
 * short and a first-time user is better served seeing the whole shape of a
 * broadcast than being walked through it blind. Only the sending settings are
 * folded away, since their default needs no decision.
 */
import { useEffect, useMemo, useState } from 'react'
import { Badge, Button, Card, Field, Input, Select, Toggle } from '@/components/ui'
import { formatDurationKo } from '@/services/format'
import { useTransport } from '../TransportContext'
import {
  AUTO_SETTINGS, DAY_NAMES, EVERY_DAY, PRIVACY_LABELS, WEEKDAYS, dayOn,
  emptySchedule, toggleDayMask,
} from '../cloud'
import type {
  BroadcastDetail, CloudMedia, NewItem, Privacy, Schedule, StreamDestination, StreamSettings,
} from '../cloud'

/** A playlist row being edited: a video plus how it should play. */
type Row = NewItem & { filename: string; duration_secs: number }

export function BroadcastForm({
  editing, onDone, onCancel,
}: {
  /** The broadcast being changed, or nothing for a new one. */
  editing?: BroadcastDetail
  onDone: () => void
  onCancel: () => void
}) {
  const t = useTransport()
  const [media, setMedia] = useState<CloudMedia[]>([])
  const [destinations, setDestinations] = useState<StreamDestination[]>([])

  const [name, setName] = useState(editing?.name ?? '')
  const [title, setTitle] = useState(editing?.title ?? '')
  const [description, setDescription] = useState(editing?.description ?? '')
  const [tags, setTags] = useState(editing?.tags ?? '')
  const [category, setCategory] = useState(editing?.category ?? '')
  const [privacy, setPrivacy] = useState<Privacy>(editing?.privacy ?? 'private')
  const [destinationId, setDestinationId] = useState(editing?.destination_id ?? '')
  const [loopAll, setLoopAll] = useState(editing?.loop_forever ?? true)
  const [settings, setSettings] = useState<StreamSettings>(editing?.settings ?? AUTO_SETTINGS)
  const [advanced, setAdvanced] = useState(false)
  const [schedule, setSchedule] = useState<Schedule>(editing?.schedule ?? emptySchedule())
  const [rows, setRows] = useState<Row[]>(
    (editing?.items ?? []).map((i) => ({
      media_id: i.media_id,
      enabled: i.enabled,
      repeat_count: i.repeat_count,
      filename: i.filename,
      duration_secs: i.duration_secs,
    })),
  )
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [dragging, setDragging] = useState<number | null>(null)

  useEffect(() => {
    let live = true
    Promise.all([t.listMedia(), t.listDestinations()])
      .then(([m, d]) => {
        if (!live) return
        setMedia(m)
        setDestinations(d)
        setDestinationId((current) => current || d[0]?.id || '')
      })
      .catch(() => undefined)
    return () => {
      live = false
    }
  }, [t])

  const ready = media.filter((m) => m.state === 'ready')
  const unused = ready.filter((m) => !rows.some((r) => r.media_id === m.id))
  const totalSecs = useMemo(
    () => rows.filter((r) => r.enabled).reduce((n, r) => n + r.duration_secs * r.repeat_count, 0),
    [rows],
  )

  function move(from: number, to: number) {
    if (to < 0 || to >= rows.length || from === to) return
    const next = rows.slice()
    const [row] = next.splice(from, 1)
    if (row) next.splice(to, 0, row)
    setRows(next)
  }

  function add(m: CloudMedia) {
    setRows([
      ...rows,
      { media_id: m.id, enabled: true, repeat_count: 1, filename: m.filename, duration_secs: m.duration_secs },
    ])
  }

  async function save() {
    setBusy(true)
    setError(null)
    try {
      const items: NewItem[] = rows.map((r) => ({
        media_id: r.media_id,
        enabled: r.enabled,
        repeat_count: Math.min(100, Math.max(1, r.repeat_count)),
      }))
      if (editing) {
        await t.updateBroadcast(editing.id, {
          name: name.trim() || '새 방송',
          title: title.trim() || name.trim(),
          description,
          tags,
          category,
          privacy,
          loop_forever: loopAll,
          destination_id: destinationId,
          settings,
          schedule,
        })
        await t.replaceItems(editing.id, items)
      } else {
        await t.createBroadcast({
          name: name.trim() || '새 방송',
          destination_id: destinationId,
          items,
          loop_forever: loopAll,
          title: title.trim() || name.trim(),
          description,
          tags,
          category,
          privacy,
          settings,
          schedule,
        })
      }
      onDone()
    } catch (e) {
      setError(e instanceof Error ? e.message : '저장할 수 없습니다.')
    } finally {
      setBusy(false)
    }
  }

  const destination = destinations.find((d) => d.id === destinationId)

  return (
    <div className="space-y-4" data-testid="broadcast-form">
      <div className="flex items-center justify-between">
        <h1 className="text-lg font-semibold">{editing ? '방송 수정' : '방송 만들기'}</h1>
        <div className="flex gap-2">
          <Button onClick={onCancel}>취소</Button>
          <Button
            variant="primary"
            onClick={save}
            disabled={busy || rows.length === 0 || !destinationId}
          >
            {busy ? '저장 중…' : editing ? '저장' : '방송 만들기'}
          </Button>
        </div>
      </div>

      {error && (
        <p role="alert" className="rounded-md border border-live-dim bg-ink-850 px-4 py-3 text-sm text-live">
          {error}
        </p>
      )}

      {/* 1 — what this broadcast is */}
      <Card title="1. 방송 정보">
        <Field label="이름" hint="목록에서 구분하기 위한 이름입니다.">
          <Input value={name} aria-label="이름" onChange={(e) => setName(e.target.value)} placeholder="밤 라디오" />
        </Field>
        <Field label="제목">
          <Input value={title} aria-label="제목" onChange={(e) => setTitle(e.target.value)} placeholder="Lofi Jazz 24/7" />
        </Field>
        <Field label="설명">
          <textarea
            aria-label="설명"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            rows={3}
            className="w-full rounded-md border border-ink-600 bg-ink-900 px-3 py-2 text-sm text-ink-100 outline-none focus:border-ink-400"
          />
        </Field>
        <div className="grid gap-3 sm:grid-cols-3">
          <Field label="태그" hint="쉼표로 구분">
            <Input value={tags} aria-label="태그" onChange={(e) => setTags(e.target.value)} placeholder="jazz, lofi" />
          </Field>
          <Field label="카테고리">
            <Input value={category} aria-label="카테고리" onChange={(e) => setCategory(e.target.value)} placeholder="Music" />
          </Field>
          <Field label="공개 범위">
            <Select
              aria-label="공개 범위"
              value={privacy}
              onChange={(e) => setPrivacy(e.target.value as Privacy)}
            >
              {(Object.keys(PRIVACY_LABELS) as Privacy[]).map((p) => (
                <option key={p} value={p}>
                  {PRIVACY_LABELS[p]}
                </option>
              ))}
            </Select>
          </Field>
        </div>
        <p className="mt-1 text-xs text-warn">
          이 정보는 247streams에 저장됩니다. 스트림 키만 등록된 대상에서는 YouTube의 제목·설명·공개
          범위를 바꾸지 않습니다 — YouTube Studio에서 직접 설정하세요.
        </p>
      </Card>

      {/* 2 — the playlist */}
      <Card
        title="2. 영상"
        action={
          <span className="text-xs text-ink-400">
            {rows.length}개 · 한 바퀴 {formatDurationKo(totalSecs)}
          </span>
        }
      >
        {rows.length === 0 ? (
          <p className="py-4 text-center text-sm text-ink-500">아래에서 영상을 추가하세요.</p>
        ) : (
          <ul className="divide-y divide-ink-700">
            {rows.map((r, i) => (
              <li
                key={`${r.media_id}-${i}`}
                data-testid="playlist-row"
                draggable
                onDragStart={() => setDragging(i)}
                onDragOver={(e) => e.preventDefault()}
                onDrop={() => {
                  if (dragging !== null) move(dragging, i)
                  setDragging(null)
                }}
                className={`flex items-center gap-3 py-2.5 ${dragging === i ? 'opacity-50' : ''}`}
              >
                <span className="cursor-grab select-none text-ink-500" aria-hidden>
                  ⠿
                </span>
                <span className="w-6 text-center font-mono text-xs text-ink-500">{i + 1}</span>
                <span className="min-w-0 flex-1 truncate text-sm text-ink-100">{r.filename}</span>
                <span className="font-mono text-xs text-ink-500">{formatDurationKo(r.duration_secs)}</span>
                <label className="flex items-center gap-1 text-xs text-ink-400">
                  반복
                  <Input
                    type="number"
                    min={1}
                    max={100}
                    aria-label={`${r.filename} 반복 횟수`}
                    value={r.repeat_count}
                    // Not clamped while typing: forcing a 1 into an emptied box
                    // makes it impossible to replace the number. The clamp is
                    // applied when it is saved.
                    onChange={(e) =>
                      setRows(
                        rows.map((x, j) =>
                          j === i
                            ? { ...x, repeat_count: Math.min(100, Math.max(0, Number(e.target.value) || 0)) }
                            : x,
                        ),
                      )
                    }
                    className="w-16"
                  />
                </label>
                <input
                  type="checkbox"
                  aria-label={`${r.filename} 사용`}
                  checked={r.enabled}
                  onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, enabled: e.target.checked } : x)))}
                />
                <Button size="sm" aria-label={`${r.filename} 위로`} onClick={() => move(i, i - 1)}>
                  ↑
                </Button>
                <Button size="sm" aria-label={`${r.filename} 아래로`} onClick={() => move(i, i + 1)}>
                  ↓
                </Button>
                <Button size="sm" aria-label={`${r.filename} 제거`} onClick={() => setRows(rows.filter((_, j) => j !== i))}>
                  ✕
                </Button>
              </li>
            ))}
          </ul>
        )}

        <div className="mt-4 border-t border-ink-700 pt-3">
          <Toggle
            checked={loopAll}
            onChange={setLoopAll}
            label="전체 반복"
            hint="끝나면 처음부터 다시 재생합니다. 24시간 방송에는 켜 두세요."
          />
        </div>

        {unused.length > 0 && (
          <div className="mt-3 border-t border-ink-700 pt-3">
            <p className="mb-2 text-xs uppercase tracking-widest text-ink-500">추가할 수 있는 영상</p>
            <div className="flex flex-wrap gap-2">
              {unused.map((m) => (
                <Button key={m.id} size="sm" onClick={() => add(m)} data-testid="add-media">
                  + {m.filename}
                </Button>
              ))}
            </div>
          </div>
        )}
        {ready.length === 0 && (
          <p className="mt-3 text-xs text-warn">사용 가능한 영상이 없습니다. 먼저 영상 탭에서 업로드하세요.</p>
        )}
      </Card>

      {/* 3 — where it goes */}
      <Card title="3. 송출 대상">
        <Field label="대상">
          <Select
            aria-label="송출 대상"
            value={destinationId}
            onChange={(e) => setDestinationId(e.target.value)}
          >
            {destinations.map((d) => (
              <option key={d.id} value={d.id}>
                {d.label}
              </option>
            ))}
          </Select>
        </Field>
        {destination && (
          <p className="font-mono text-xs text-ink-500">
            {destination.rtmps_url} · {destination.key_masked}
          </p>
        )}
      </Card>

      {/* 4 — how it is sent */}
      <Card
        title="4. 송출 설정"
        action={
          <Button size="sm" onClick={() => setAdvanced(!advanced)}>
            {advanced ? '간단히' : '고급 설정'}
          </Button>
        }
      >
        {!advanced ? (
          <p className="text-sm text-ink-400">
            자동 — 서버가 영상에 맞는 안정적인 설정으로 송출합니다. <Badge tone="ok">권장</Badge>
          </p>
        ) : (
          <>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="해상도">
                <Select
                  aria-label="해상도"
                  value={settings.resolution}
                  onChange={(e) =>
                    setSettings({ ...settings, resolution: e.target.value as StreamSettings['resolution'] })
                  }
                >
                  <option value="auto">자동</option>
                  <option value="720p">720p</option>
                  <option value="1080p">1080p</option>
                </Select>
              </Field>
              <Field label="프레임레이트">
                <Select
                  aria-label="프레임레이트"
                  value={settings.fps}
                  onChange={(e) => setSettings({ ...settings, fps: e.target.value as StreamSettings['fps'] })}
                >
                  <option value="auto">자동</option>
                  <option value="30">30</option>
                  <option value="60">60</option>
                </Select>
              </Field>
              <Field label="영상 비트레이트 (kbps)" hint="0은 자동">
                <Input
                  type="number"
                  aria-label="영상 비트레이트"
                  value={settings.video_bitrate_kbps}
                  onChange={(e) => setSettings({ ...settings, video_bitrate_kbps: Number(e.target.value) || 0 })}
                />
              </Field>
              <Field label="음성 비트레이트 (kbps)" hint="0은 자동">
                <Input
                  type="number"
                  aria-label="음성 비트레이트"
                  value={settings.audio_bitrate_kbps}
                  onChange={(e) => setSettings({ ...settings, audio_bitrate_kbps: Number(e.target.value) || 0 })}
                />
              </Field>
            </div>
            <p className="mt-2 text-xs text-warn">
              현재 송출은 업로드할 때 준비된 1080p30 파일을 그대로 보냅니다(스트림 복사). 자동이 아닌
              값은 저장되며, 재인코딩 송출이 추가되면 적용됩니다.
            </p>
          </>
        )}
      </Card>

      {/* 5 — when */}
      <Card title="5. 예약">
        <Toggle
          checked={schedule.enabled}
          onChange={(enabled) => setSchedule({ ...schedule, enabled })}
          label="예약 시작"
          hint="브라우저를 닫아도 서버가 시간을 지켜 시작합니다."
        />
        {schedule.enabled && (
          <div className="mt-3 space-y-3">
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="시작" hint={schedule.timezone}>
                <Input
                  type="datetime-local"
                  aria-label="시작 시각"
                  value={toLocalInput(schedule.start_at)}
                  onChange={(e) => setSchedule({ ...schedule, start_at: fromLocalInput(e.target.value) })}
                />
              </Field>
              <Field label="종료 (선택)">
                <Input
                  type="datetime-local"
                  aria-label="종료 시각"
                  value={toLocalInput(schedule.stop_at)}
                  onChange={(e) => setSchedule({ ...schedule, stop_at: fromLocalInput(e.target.value) })}
                />
              </Field>
            </div>
            <div>
              <p className="mb-2 text-sm text-ink-100">반복</p>
              <div className="flex flex-wrap gap-2">
                <Button size="sm" onClick={() => setSchedule({ ...schedule, repeat_days: 0 })}>
                  한 번만
                </Button>
                <Button size="sm" onClick={() => setSchedule({ ...schedule, repeat_days: EVERY_DAY })}>
                  매일
                </Button>
                <Button size="sm" onClick={() => setSchedule({ ...schedule, repeat_days: WEEKDAYS })}>
                  주중
                </Button>
                {DAY_NAMES.map((d, i) => (
                  <Button
                    key={d}
                    size="sm"
                    aria-label={`${d}요일`}
                    aria-pressed={dayOn(schedule.repeat_days, i)}
                    variant={dayOn(schedule.repeat_days, i) ? 'primary' : 'ghost'}
                    onClick={() => setSchedule({ ...schedule, repeat_days: toggleDayMask(schedule.repeat_days, i) })}
                  >
                    {d}
                  </Button>
                ))}
              </div>
            </div>
          </div>
        )}
      </Card>

      {/* 6 — the summary, so nothing is a surprise */}
      <Card title="6. 확인">
        <dl className="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-2">
          <Row label="이름" value={name || '새 방송'} />
          <Row label="공개 범위" value={PRIVACY_LABELS[privacy]} />
          <Row label="영상" value={`${rows.filter((r) => r.enabled).length}개 · ${formatDurationKo(totalSecs)}`} />
          <Row label="전체 반복" value={loopAll ? '켜짐' : '한 바퀴만'} />
          <Row label="대상" value={destination?.label ?? '—'} />
          <Row label="송출 설정" value={settings.resolution === 'auto' ? '자동' : `${settings.resolution} ${settings.fps}fps`} />
        </dl>
        <p className="mt-3 text-xs text-ink-500">
          만든 뒤 대시보드에서 시작합니다. 시작하면 브라우저를 닫아도 서버에서 계속 송출됩니다.
        </p>
      </Card>
    </div>
  )
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-4 border-b border-ink-800 py-1">
      <dt className="text-ink-500">{label}</dt>
      <dd className="text-ink-100">{value}</dd>
    </div>
  )
}

/** UTC in the row, the reader's own clock in the box. */
function toLocalInput(utc?: string | null): string {
  if (!utc) return ''
  const d = new Date(utc)
  if (Number.isNaN(d.getTime())) return ''
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`
}

function fromLocalInput(local: string): string | null {
  if (!local) return null
  const d = new Date(local)
  return Number.isNaN(d.getTime()) ? null : d.toISOString()
}
