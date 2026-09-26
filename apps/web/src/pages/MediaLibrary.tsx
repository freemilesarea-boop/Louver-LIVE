/**
 * Upload a video, and watch it become usable.
 *
 * Same principle as the desktop's add (§1 of the earlier UX work): the file
 * appears the moment the upload lands, and the words describe what is happening
 * to it — never "최적화", never a button asking the user to decide.
 */
import { useEffect, useRef, useState } from 'react'
import { Badge, Button, Card, EmptyState, ProgressBar } from '@/components/ui'
import { formatBytes, formatDurationKo } from '@/services/format'
import { useTransport } from '../TransportContext'
import { MEDIA_LABELS } from '../cloud'
import type { CloudMedia } from '../cloud'

const BUSY: CloudMedia['state'][] = ['uploaded', 'analysing', 'preparing']

export function MediaLibrary() {
  const t = useTransport()
  const [items, setItems] = useState<CloudMedia[]>([])
  const [uploading, setUploading] = useState<{ name: string; fraction: number } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const input = useRef<HTMLInputElement>(null)

  async function refresh() {
    try {
      setItems(await t.listMedia())
    } catch {
      /* the next tick will try again */
    }
  }

  useEffect(() => {
    let live = true
    refresh()
    // Only while something is still being prepared: a quiet library does not
    // need polling.
    const timer = setInterval(() => {
      if (live) refresh()
    }, 4000)
    return () => {
      live = false
      clearInterval(timer)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t])

  async function upload(file: File) {
    setError(null)
    setUploading({ name: file.name, fraction: 0 })
    try {
      await t.uploadMedia(file, (fraction) => setUploading({ name: file.name, fraction }))
      await refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : '업로드에 실패했습니다.')
    } finally {
      setUploading(null)
      if (input.current) input.current.value = ''
    }
  }

  return (
    <Card
      title="영상"
      action={
        <Button variant="primary" size="sm" onClick={() => input.current?.click()} disabled={!!uploading}>
          영상 추가
        </Button>
      }
    >
      <input
        ref={input}
        type="file"
        accept="video/*"
        aria-label="영상 파일"
        className="hidden"
        onChange={(e) => {
          const f = e.target.files?.[0]
          if (f) upload(f)
        }}
      />

      {uploading && (
        <div className="mb-4">
          <ProgressBar percent={uploading.fraction * 100} label={`${uploading.name} 업로드 중`} />
        </div>
      )}

      {error && (
        <p role="alert" className="mb-3 text-sm text-live">
          {error}
        </p>
      )}

      {items.length === 0 ? (
        <EmptyState title="아직 영상이 없습니다" hint="영상을 추가하면 자동으로 검사하고 필요한 부분만 변환합니다." />
      ) : (
        <ul className="divide-y divide-ink-700">
          {items.map((m) => (
            <li key={m.id} className="flex items-center justify-between gap-4 py-3" data-testid="media-row">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span className="truncate text-sm text-ink-100">{m.filename}</span>
                  <Badge tone={m.state === 'ready' ? 'ok' : m.state === 'failed' ? 'live' : 'warn'}>
                    {MEDIA_LABELS[m.state]}
                  </Badge>
                </div>
                <div className="mt-1 text-xs text-ink-500">
                  {formatBytes(m.size_bytes)}
                  {m.duration_secs > 0 ? ` · ${formatDurationKo(m.duration_secs)}` : ''}
                  {m.width > 0 ? ` · ${m.width}×${m.height}` : ''}
                  {m.last_error ? ` · ${m.last_error}` : ''}
                </div>
              </div>
              <Button
                size="sm"
                disabled={BUSY.includes(m.state)}
                onClick={async () => {
                  setError(null)
                  try {
                    await t.deleteMedia(m.id)
                    await refresh()
                  } catch (e) {
                    setError(e instanceof Error ? e.message : '삭제할 수 없습니다.')
                  }
                }}
              >
                삭제
              </Button>
            </li>
          ))}
        </ul>
      )}
    </Card>
  )
}
