import { useEffect, useState } from 'react'
import { AlertTriangle, Cpu, Play, Radio, Square, Wifi } from 'lucide-react'
import { useAppStore } from '@/stores/useAppStore'
import { api } from '@/services/ipc'
import { formatDuration, formatMbps } from '@/services/format'
import { Badge, Button, Card, Modal, Stat } from '@/components/ui'
import { StatusPill } from '@/components/StatusPill'
import type { PreflightReport } from '@/types'

/** Main dashboard (§24, §26, §28). */
export function Dashboard() {
  const {
    status, metrics, settings, activePlaylist, activePlaylistId,
    refreshStatus, refreshMetrics, reportError, toast, setPage,
  } = useAppStore()

  const [preflight, setPreflight] = useState<PreflightReport | null>(null)
  const [confirmStop, setConfirmStop] = useState(false)
  const [warnings, setWarnings] = useState<string[] | null>(null)
  const [pendingStart, setPendingStart] = useState<'live' | 'test' | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    const t = setInterval(() => {
      void refreshStatus()
      void refreshMetrics()
    }, 1000)
    return () => clearInterval(t)
  }, [refreshStatus, refreshMetrics])

  const state = status?.supervisor.state ?? 'IDLE'
  const isLive = state === 'LIVE' || state === 'CONNECTING' || state === 'RECONNECTING' || state === 'PREPARING'
  const sup = status?.supervisor

  async function beginStart(kind: 'live' | 'test') {
    if (activePlaylistId == null) {
      toast({ kind: 'error', message: '먼저 플레이리스트를 선택해주세요.' })
      setPage('playlist')
      return
    }
    setBusy(true)
    try {
      const report = await api.runPreflight(activePlaylistId, kind === 'test')
      setPreflight(report)
      if (!report.can_broadcast) return
      // §62: the uptime warnings are shown before the first real broadcast.
      if (kind === 'live' && settings && !localStorage.getItem('louver.warned')) {
        setWarnings(await api.uptimeWarnings())
        setPendingStart(kind)
        return
      }
      await doStart(kind)
    } catch (e) {
      reportError(e)
    } finally {
      setBusy(false)
    }
  }

  async function doStart(kind: 'live' | 'test') {
    if (activePlaylistId == null) return
    setBusy(true)
    try {
      if (kind === 'live') {
        await api.startBroadcast(activePlaylistId)
        toast({ kind: 'success', message: '방송을 시작했습니다.' })
      } else {
        await api.startDryRun(activePlaylistId)
        toast({ kind: 'info', message: '로컬 테스트 송출을 시작했습니다.' })
      }
      setPreflight(null)
      await refreshStatus()
    } catch (e) {
      reportError(e)
    } finally {
      setBusy(false)
    }
  }

  async function doStop() {
    setConfirmStop(false)
    setBusy(true)
    try {
      await api.stopBroadcast()
      toast({ kind: 'info', message: '방송을 종료했습니다.' })
      await refreshStatus()
    } catch (e) {
      reportError(e)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold text-ink-100">대시보드</h1>
          <p className="mt-1 text-xs text-ink-500">
            {status?.next_scheduled_start
              ? `다음 예약 방송: ${status.next_scheduled_start}`
              : '예약된 방송이 없습니다'}
          </p>
        </div>
        <StatusPill state={state} dryRun={status?.dry_run} />
      </div>

      {/* Primary control (§26): the largest thing on the screen. */}
      <Card>
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            <div className="text-xs uppercase tracking-wider text-ink-500">현재 플레이리스트</div>
            <div className="mt-1 truncate text-xl text-ink-100">
              {status?.playlist_name ?? activePlaylist?.playlist.name ?? '선택되지 않음'}
            </div>
            <div className="mt-1 text-xs text-ink-500">
              {/* item_count is 0 while idle, so `??` would keep the zero. The
                  selected playlist is the right source when nothing is live. */}
              {(isLive ? status?.item_count : activePlaylist?.items.length) ?? 0}개 영상
              {status?.dry_run && <span className="ml-2 text-warn">· 로컬 테스트 모드</span>}
            </div>
          </div>
          <div className="flex shrink-0 gap-2">
            {!isLive ? (
              <>
                <Button variant="ghost" onClick={() => void beginStart('test')} disabled={busy}>
                  <span className="inline-flex items-center gap-2"><Radio size={15} /> 로컬 테스트</span>
                </Button>
                <Button variant="live" size="lg" onClick={() => void beginStart('live')} disabled={busy}>
                  <span className="inline-flex items-center gap-2"><Play size={17} /> 방송 시작</span>
                </Button>
              </>
            ) : (
              <Button variant="danger" size="lg" onClick={() => setConfirmStop(true)} disabled={busy}>
                <span className="inline-flex items-center gap-2"><Square size={15} /> 방송 종료</span>
              </Button>
            )}
          </div>
        </div>
      </Card>

      {state === 'RECONNECTING' && (
        <div className="flex items-start gap-3 rounded-lg border border-warn-dim bg-warn-dim/10 p-4">
          <AlertTriangle size={18} className="mt-0.5 shrink-0 text-warn" />
          <div className="text-sm">
            <p className="text-warn">인터넷 연결이 끊어졌습니다. 자동으로 다시 연결하고 있습니다.</p>
            {sup?.next_retry_in_secs != null && (
              <p className="mt-1 text-xs text-ink-400">{sup.next_retry_in_secs}초 후 재시도 · 재연결 {sup.reconnect_count}회</p>
            )}
          </div>
        </div>
      )}

      <div className="grid gap-4 lg:grid-cols-2">
        <Card title="방송 상태">
          <div className="grid grid-cols-2 gap-5">
            <Stat label="현재 영상" value={<span className="text-sm">{status?.current_item ?? '—'}</span>} />
            <Stat label="다음 영상" value={<span className="text-sm text-ink-400">{status?.next_item ?? '—'}</span>} />
            <Stat label="전체 방송 시간" value={formatDuration(status?.elapsed_secs ?? 0)} tone={isLive ? 'live' : 'default'} />
            <Stat
              label="예약 종료까지"
              value={status?.remaining_secs != null ? formatDuration(status.remaining_secs) : '—'}
            />
            <Stat label="업로드 속도" value={formatMbps(sup?.progress.bitrate_kbps ?? 0)} />
            <Stat label="재연결 횟수" value={sup?.reconnect_count ?? 0} tone={(sup?.reconnect_count ?? 0) > 0 ? 'warn' : 'default'} />
          </div>
          <div className="mt-5 flex flex-wrap items-center gap-2 border-t border-ink-700 pt-4">
            <Badge tone={sup?.mode === 'stream_copy' ? 'ok' : 'warn'}>
              {sup?.mode === 'stream_copy' ? 'STREAM COPY' : 'COMPATIBILITY ENCODE'}
            </Badge>
            <Badge tone={sup?.pid ? 'ok' : 'default'}>FFmpeg {sup?.pid ? 'Running' : 'Stopped'}</Badge>
            {metrics?.sleep_prevented && <Badge tone="ok">절전 차단 중</Badge>}
            {status?.start_reason && (
              <Badge>{{ manual: '수동 시작', scheduled: '예약 시작', recovered: '자동 복구' }[status.start_reason]}</Badge>
            )}
          </div>
        </Card>

        <Card title="시스템">
          <div className="grid grid-cols-2 gap-5">
            <Stat label="앱 CPU" value={`${(metrics?.app_cpu_percent ?? 0).toFixed(1)}%`} />
            <Stat
              label="FFmpeg CPU"
              value={`${(metrics?.ffmpeg_cpu_percent ?? 0).toFixed(1)}%`}
              tone={(metrics?.ffmpeg_cpu_percent ?? 0) > 60 ? 'warn' : 'default'}
            />
            <Stat label="앱 메모리" value={metrics?.app_memory_label ?? '—'} />
            <Stat label="캐시 크기" value={metrics?.cache_label ?? '—'} />
            <Stat label="여유 공간" value={metrics?.free_disk_label ?? '—'} />
            <Stat label="네트워크" value={<span className="inline-flex items-center gap-1.5 text-sm"><Wifi size={14} />{isLive ? '전송 중' : '대기'}</span>} />
          </div>
          <p className="mt-5 flex items-start gap-2 border-t border-ink-700 pt-4 text-[11px] leading-relaxed text-ink-500">
            <Cpu size={13} className="mt-0.5 shrink-0" />
            Stream Copy 모드에서는 방송 중 영상을 재인코딩하지 않습니다.
          </p>
        </Card>
      </div>

      {/* Preflight (§29) */}
      <Modal
        open={preflight != null && !preflight.can_broadcast}
        title="방송 시작 전 점검"
        onClose={() => setPreflight(null)}
        footer={<Button onClick={() => setPreflight(null)}>닫기</Button>}
      >
        <ul className="space-y-2">
          {preflight?.checks.map((c) => (
            <li key={c.id} className="flex items-start gap-3">
              <span
                className={`mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full ${
                  c.outcome === 'pass' ? 'bg-ok' : c.outcome === 'warn' ? 'bg-warn' : 'bg-live'
                }`}
              />
              <span className="min-w-0">
                <span className="text-ink-100">{c.label}</span>
                <span className="ml-2 text-xs text-ink-400">{c.detail}</span>
                {c.code && <span className="ml-2 font-mono text-[10px] text-ink-600">{c.code}</span>}
              </span>
            </li>
          ))}
        </ul>
      </Modal>

      {/* §62 warnings shown once before the first live broadcast */}
      <Modal
        open={warnings != null}
        title="방송을 시작하기 전에"
        onClose={() => { setWarnings(null); setPendingStart(null) }}
        footer={
          <>
            <Button onClick={() => { setWarnings(null); setPendingStart(null) }}>취소</Button>
            <Button
              variant="live"
              onClick={() => {
                localStorage.setItem('louver.warned', '1')
                const k = pendingStart
                setWarnings(null)
                setPendingStart(null)
                if (k) void doStart(k)
              }}
            >
              확인하고 시작
            </Button>
          </>
        }
      >
        <ul className="list-disc space-y-2 pl-5">
          {warnings?.map((w) => <li key={w}>{w}</li>)}
        </ul>
      </Modal>

      {/* §26: stopping requires confirmation */}
      <Modal
        open={confirmStop}
        title="방송을 종료할까요?"
        onClose={() => setConfirmStop(false)}
        footer={
          <>
            <Button onClick={() => setConfirmStop(false)}>취소</Button>
            <Button variant="danger" onClick={() => void doStop()}>방송 종료</Button>
          </>
        }
      >
        종료하면 YouTube 스트림이 즉시 중단됩니다. 예약이 설정되어 있다면 다음 예약 시간에 다시 시작됩니다.
      </Modal>
    </div>
  )
}
