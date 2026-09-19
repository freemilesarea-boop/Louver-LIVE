import { useEffect, useMemo, useState } from 'react'
import { GripVertical, Plus, Trash2, Wand2, X } from 'lucide-react'
import { useAppStore } from '@/stores/useAppStore'
import { api, pickVideoFiles } from '@/services/ipc'
import { formatBytes, formatDurationKo, formatResolution, isBroadcastReady, mediaStatusLabel } from '@/services/format'
import { Badge, Button, Card, EmptyState, Modal, ProgressBar, Select } from '@/components/ui'
import type { DiskEstimate, PlaylistItemView } from '@/types'

/** Playlist page with drag-and-drop ordering (§11, §25). */
export function PlaylistPage() {
  const {
    playlists, activePlaylist, activePlaylistId, setActivePlaylist,
    refreshPlaylists, refreshActivePlaylist, refreshMedia, normalizing,
    reportError, toast,
  } = useAppStore()

  const [dragId, setDragId] = useState<number | null>(null)
  const [overId, setOverId] = useState<number | null>(null)
  const [estimate, setEstimate] = useState<DiskEstimate | null>(null)
  const [reasons, setReasons] = useState<{ name: string; list: string[] } | null>(null)
  const [newName, setNewName] = useState('')
  const [creating, setCreating] = useState(false)

  useEffect(() => { void refreshPlaylists() }, [refreshPlaylists])

  const items = useMemo(() => activePlaylist?.items ?? [], [activePlaylist])
  const unready = useMemo(
    () => items.filter((i) => i.enabled && !isBroadcastReady(i.media.status)),
    [items],
  )

  async function addVideos() {
    try {
      const paths = await pickVideoFiles()
      if (!paths.length) return
      const result = await api.importMedia(paths)
      if (activePlaylistId != null && result.imported.length) {
        await api.addToPlaylist(activePlaylistId, result.imported.map((m) => m.id))
      }
      for (const f of result.failed) {
        toast({ kind: 'error', message: `${f.path}: ${f.message}`, code: f.code })
      }
      if (result.imported.length) {
        toast({ kind: 'success', message: `${result.imported.length}개 영상을 추가했습니다.` })
      }
      await Promise.all([refreshMedia(), refreshActivePlaylist()])
    } catch (e) {
      reportError(e)
    }
  }

  /** §10: show the disk plan before any encoding starts. */
  async function askToOptimize() {
    try {
      const ids = unready.map((i) => i.media_id)
      if (!ids.length) return toast({ kind: 'info', message: '최적화할 영상이 없습니다.' })
      setEstimate(await api.estimateOptimization(ids))
    } catch (e) {
      reportError(e)
    }
  }

  async function runOptimize() {
    const ids = unready.map((i) => i.media_id)
    setEstimate(null)
    try {
      const done = await api.optimizeMedia(ids)
      toast({ kind: 'success', message: `${done}개 영상을 방송용으로 최적화했습니다.` })
    } catch (e) {
      reportError(e)
    } finally {
      await Promise.all([refreshMedia(), refreshActivePlaylist()])
    }
  }

  async function commitOrder(targetId: number) {
    if (dragId == null || dragId === targetId || activePlaylistId == null) return
    const ids = items.map((i) => i.id)
    const from = ids.indexOf(dragId)
    const to = ids.indexOf(targetId)
    if (from < 0 || to < 0) return
    ids.splice(to, 0, ids.splice(from, 1)[0]!)
    try {
      await api.reorderPlaylist(activePlaylistId, ids)
      await refreshActivePlaylist()
    } catch (e) {
      reportError(e)
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-lg font-semibold text-ink-100">플레이리스트</h1>
        <div className="flex items-center gap-2">
          <Select
            aria-label="플레이리스트 선택"
            value={activePlaylistId ?? ''}
            onChange={(e) => void setActivePlaylist(Number(e.target.value))}
            className="w-52"
          >
            <option value="" disabled>플레이리스트 선택</option>
            {playlists.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
          </Select>
          <Button onClick={() => setCreating(true)}><Plus size={15} /></Button>
        </div>
      </div>

      {activePlaylist ? (
        <>
          <Card
            title={<span>VIDEO PLAYLIST · {activePlaylist.playlist.name}</span>}
            action={
              <div className="flex items-center gap-2">
                <Select
                  aria-label="재생 모드"
                  value={activePlaylist.playlist.playback_mode}
                  onChange={async (e) => {
                    try {
                      await api.updatePlaylist(
                        activePlaylist.playlist.id,
                        activePlaylist.playlist.name,
                        e.target.value,
                        activePlaylist.playlist.output_profile,
                      )
                      await refreshActivePlaylist()
                    } catch (err) { reportError(err) }
                  }}
                  className="w-36 !py-1 text-xs"
                >
                  <option value="sequential">Sequential</option>
                  <option value="shuffle_once">Shuffle Once</option>
                </Select>
              </div>
            }
          >
            {items.length === 0 ? (
              <EmptyState
                title="아직 영상이 없습니다"
                hint="MP4, MOV, MKV 파일을 추가하면 자동으로 분석합니다."
                action={<Button variant="primary" onClick={() => void addVideos()}>+ 영상 추가</Button>}
              />
            ) : (
              <ol className="space-y-1" aria-label="영상 목록">
                {items.map((item, idx) => (
                  <PlaylistRow
                    key={item.id}
                    item={item}
                    index={idx}
                    isDragging={dragId === item.id}
                    isOver={overId === item.id}
                    onDragStart={() => setDragId(item.id)}
                    onDragOver={() => setOverId(item.id)}
                    onDrop={() => { void commitOrder(item.id); setDragId(null); setOverId(null) }}
                    onDragEnd={() => { setDragId(null); setOverId(null) }}
                    onToggle={async () => {
                      try {
                        await api.setItemEnabled(item.id, !item.enabled)
                        await refreshActivePlaylist()
                      } catch (e) { reportError(e) }
                    }}
                    onRemove={async () => {
                      try {
                        await api.removePlaylistItem(item.id)
                        await refreshActivePlaylist()
                      } catch (e) { reportError(e) }
                    }}
                    onWhy={async () => {
                      try {
                        setReasons({ name: item.media.display_name, list: await api.compatibilityReasons(item.media_id) })
                      } catch (e) { reportError(e) }
                    }}
                  />
                ))}
              </ol>
            )}

            {/* The empty state already offers its own call to action, so the
                footer only appears once there is a list to summarise. */}
            {items.length > 0 && (
              <div className="mt-4 flex flex-wrap items-center justify-between gap-3 border-t border-ink-700 pt-4">
                <Button onClick={() => void addVideos()}>+ 영상 추가</Button>
                <div className="text-xs text-ink-400">
                  전체 길이{' '}
                  <span data-testid="playlist-total" className="ml-1.5 font-mono text-ink-100">
                    {activePlaylist.total_duration_label}
                  </span>
                </div>
              </div>
            )}
          </Card>

          {unready.length > 0 && (
            <Card>
              <div className="flex flex-wrap items-center justify-between gap-3">
                <div>
                  <p className="text-sm text-warn">{unready.length}개 영상이 방송 규격과 다릅니다.</p>
                  <p className="mt-1 text-xs text-ink-500">
                    방송을 시작하려면 먼저 최적화해야 합니다. 원본 파일은 변경되지 않습니다.
                  </p>
                </div>
                <Button variant="primary" onClick={() => void askToOptimize()} disabled={!!normalizing}>
                  <span className="inline-flex items-center gap-2"><Wand2 size={15} /> 방송용으로 최적화</span>
                </Button>
              </div>
              {normalizing && (
                <div className="mt-4 space-y-2">
                  <ProgressBar
                    percent={normalizing.percent}
                    label={`${normalizing.file_name} · 남은 파일 ${normalizing.remaining_files}개`}
                  />
                  <Button size="sm" onClick={() => void api.cancelOptimization()}>중단</Button>
                </div>
              )}
            </Card>
          )}
        </>
      ) : (
        <Card>
          <EmptyState
            title="플레이리스트가 없습니다"
            hint="플레이리스트를 만들고 영상을 추가하세요."
            action={<Button variant="primary" onClick={() => setCreating(true)}>플레이리스트 만들기</Button>}
          />
        </Card>
      )}

      <Modal
        open={creating}
        title="새 플레이리스트"
        onClose={() => setCreating(false)}
        footer={
          <>
            <Button onClick={() => setCreating(false)}>취소</Button>
            <Button
              variant="primary"
              onClick={async () => {
                if (!newName.trim()) return
                try {
                  const id = await api.createPlaylist(newName.trim())
                  setNewName('')
                  setCreating(false)
                  await refreshPlaylists()
                  await setActivePlaylist(id)
                } catch (e) { reportError(e) }
              }}
            >
              만들기
            </Button>
          </>
        }
      >
        <input
          aria-label="플레이리스트 이름"
          placeholder="예: Night Jazz"
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          className="w-full rounded-md border border-ink-600 bg-ink-900 px-3 py-2 text-sm text-ink-100 outline-none focus:border-ink-400"
        />
      </Modal>

      <Modal
        open={reasons != null}
        title={`${reasons?.name ?? ''} · 최적화가 필요한 이유`}
        onClose={() => setReasons(null)}
        footer={<Button onClick={() => setReasons(null)}>닫기</Button>}
      >
        <ul className="list-disc space-y-1.5 pl-5">
          {reasons?.list.map((r) => <li key={r}>{r}</li>)}
        </ul>
      </Modal>

      {/* §10: never start an encode that could fill the disk */}
      <Modal
        open={estimate != null}
        title="저장 공간 확인"
        onClose={() => setEstimate(null)}
        footer={
          <>
            <Button onClick={() => setEstimate(null)}>취소</Button>
            <Button variant="primary" disabled={!estimate?.has_enough_space} onClick={() => void runOptimize()}>
              최적화 시작
            </Button>
          </>
        }
      >
        {estimate && (
          <dl className="space-y-2">
            <div className="flex justify-between"><dt>대상 영상</dt><dd className="font-mono">{estimate.files_to_process}개</dd></div>
            <div className="flex justify-between"><dt>예상 추가 공간</dt><dd className="font-mono">약 {formatBytes(estimate.estimated_bytes)}</dd></div>
            <div className="flex justify-between"><dt>현재 여유 공간</dt><dd className="font-mono">{formatBytes(estimate.available_bytes)}</dd></div>
            {!estimate.has_enough_space && (
              <p className="mt-3 rounded border border-live-dim bg-live-dim/10 p-3 text-live">
                저장 공간이 부족합니다. 공간을 확보한 뒤 다시 시도해주세요.
              </p>
            )}
          </dl>
        )}
      </Modal>
    </div>
  )
}

function PlaylistRow({
  item, index, isDragging, isOver, onDragStart, onDragOver, onDrop, onDragEnd, onToggle, onRemove, onWhy,
}: {
  item: PlaylistItemView
  index: number
  isDragging: boolean
  isOver: boolean
  onDragStart: () => void
  onDragOver: () => void
  onDrop: () => void
  onDragEnd: () => void
  onToggle: () => void
  onRemove: () => void
  onWhy: () => void
}) {
  const ready = isBroadcastReady(item.media.status)
  const dur = item.media.normalized_duration_secs ?? item.media.duration_secs
  return (
    <li
      draggable
      onDragStart={onDragStart}
      onDragOver={(e) => { e.preventDefault(); onDragOver() }}
      onDrop={(e) => { e.preventDefault(); onDrop() }}
      onDragEnd={onDragEnd}
      data-testid={`playlist-item-${item.id}`}
      className={`flex items-center gap-3 rounded-md border px-3 py-2.5 ${
        isDragging ? 'opacity-40' : ''
      } ${isOver ? 'border-ink-400' : 'border-transparent'} ${item.enabled ? 'bg-ink-800' : 'bg-ink-900 opacity-60'}`}
    >
      <GripVertical size={14} className="shrink-0 cursor-grab text-ink-600" aria-hidden />
      <span className="w-6 shrink-0 text-right font-mono text-xs text-ink-500">{index + 1}</span>
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm text-ink-100">{item.media.display_name}</div>
        <div className="mt-0.5 flex flex-wrap items-center gap-2 text-[11px] text-ink-500">
          <span className="font-mono">{formatDurationKo(dur)}</span>
          <span>{formatResolution(item.media.width, item.media.height)}</span>
          <span>{item.media.fps.toFixed(0)}fps</span>
          <span>{item.media.video_codec}</span>
          {item.media.is_hdr && <Badge tone="warn">HDR</Badge>}
        </div>
      </div>
      {ready ? (
        <Badge tone="ok">{mediaStatusLabel(item.media.status)}</Badge>
      ) : (
        <button onClick={onWhy} className="text-[11px] text-warn underline-offset-2 hover:underline">
          {mediaStatusLabel(item.media.status)}
        </button>
      )}
      <button
        onClick={onToggle}
        aria-label={item.enabled ? '이 영상 제외' : '이 영상 포함'}
        className="rounded p-1 text-ink-500 hover:text-ink-200"
      >
        {item.enabled ? <X size={14} /> : <Plus size={14} />}
      </button>
      <button onClick={onRemove} aria-label="목록에서 삭제" className="rounded p-1 text-ink-600 hover:text-live">
        <Trash2 size={14} />
      </button>
    </li>
  )
}
