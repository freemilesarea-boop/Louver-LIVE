import { useEffect, useState } from 'react'
import { FolderOpen, RefreshCw } from 'lucide-react'
import { useAppStore } from '@/stores/useAppStore'
import { api, revealPath } from '@/services/ipc'
import { Badge, Button, Card } from '@/components/ui'
import type { StreamEvent } from '@/types'

const TARGETS = [
  { id: 'app', label: 'app.log' },
  { id: 'stream', label: 'stream.log' },
  { id: 'ffmpeg', label: 'ffmpeg.log' },
] as const

/** Logs page (§34, §61). */
export function LogsPage() {
  const { reportError, settings } = useAppStore()
  const [target, setTarget] = useState<string>('stream')
  const [lines, setLines] = useState<string[]>([])
  const [events, setEvents] = useState<StreamEvent[]>([])

  async function load() {
    try {
      const [l, e] = await Promise.all([api.readLog(target, 400), api.recentEvents(100)])
      setLines(l)
      setEvents(e)
    } catch (err) {
      reportError(err)
    }
  }

  useEffect(() => {
    void load()
    const t = setInterval(() => void load(), 4000)
    return () => clearInterval(t)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target])

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-lg font-semibold text-ink-100">로그</h1>
        <div className="flex gap-2">
          <Button size="sm" onClick={() => void load()}><RefreshCw size={14} /></Button>
          <Button
            size="sm"
            onClick={() => settings && void revealPath(settings.logs_dir).catch(reportError)}
          >
            <span className="inline-flex items-center gap-2"><FolderOpen size={14} /> 로그 폴더 열기</span>
          </Button>
        </div>
      </div>

      <Card title="최근 이벤트">
        <ul data-testid="recent-events" className="max-h-64 space-y-1 overflow-auto">
          {events.length === 0 && <li className="py-6 text-center text-xs text-ink-500">기록된 이벤트가 없습니다.</li>}
          {events.map((e) => (
            <li key={e.id} className="flex items-start gap-3 py-1 text-xs">
              <span className="shrink-0 font-mono text-ink-600">{e.at.slice(5, 19)}</span>
              <Badge tone={e.level === 'error' ? 'live' : e.level === 'warn' ? 'warn' : 'default'}>
                {e.level.toUpperCase()}
              </Badge>
              {e.code && <span className="shrink-0 font-mono text-[10px] text-ink-600">{e.code}</span>}
              <span className="min-w-0 text-ink-300">{e.message}</span>
            </li>
          ))}
        </ul>
      </Card>

      <Card
        title="파일 로그"
        action={
          <div className="flex gap-1">
            {TARGETS.map((t) => (
              <button
                key={t.id}
                onClick={() => setTarget(t.id)}
                className={`rounded px-2 py-1 font-mono text-[11px] ${
                  target === t.id ? 'bg-ink-700 text-ink-100' : 'text-ink-500 hover:text-ink-300'
                }`}
              >
                {t.label}
              </button>
            ))}
          </div>
        }
      >
        <pre className="max-h-[26rem] overflow-auto whitespace-pre-wrap break-all rounded bg-ink-950 p-3 font-mono text-[11px] leading-relaxed text-ink-400">
          {lines.length ? lines.join('\n') : '(비어 있음)'}
        </pre>
        <p className="mt-2 text-[11px] text-ink-600">
          스트림 키는 로그에 기록되지 않습니다. 최대 10MB × 5개까지 보관됩니다.
        </p>
      </Card>
    </div>
  )
}
