/**
 * Where this is running, and what it is costing.
 *
 * The banner is the first thing on the page because it answers the question a
 * user is about to get wrong: on `local` the broadcast dies with the laptop,
 * on `cloud` it does not. The rest is what a 24-hour test needs — per
 * broadcast, and for the machine underneath.
 */
import { useEffect, useState } from 'react'
import { Badge, Card, Stat } from '@/components/ui'
import { formatBytes, formatDurationKo } from '@/services/format'
import { useTransport } from '../TransportContext'
import { DEPLOYMENT_LABELS, RUNTIME_LABELS } from '../cloud'
import type { Health, Metrics } from '../cloud'

export function DeploymentBanner() {
  const t = useTransport()
  const [health, setHealth] = useState<Health | null>(null)

  useEffect(() => {
    let live = true
    const read = () => {
      t.health()
        .then((h) => live && setHealth(h))
        .catch(() => live && setHealth(null))
    }
    read()
    const timer = setInterval(read, 30000)
    return () => {
      live = false
      clearInterval(timer)
    }
  }, [t])

  if (!health) return null
  const label = DEPLOYMENT_LABELS[health.deployment]
  const cloud = health.deployment === 'cloud'
  const failing = Object.entries(health.checks).filter(([, ok]) => !ok)

  return (
    <div
      data-testid="deployment-banner"
      data-deployment={health.deployment}
      className={`border-b px-6 py-2 text-xs ${
        cloud ? 'border-ok-dim bg-ok-dim/10 text-ok' : 'border-warn-dim bg-warn-dim/10 text-warn'
      }`}
    >
      <span className="font-semibold tracking-widest">{label.title}</span>
      <span className="ml-3 text-ink-300">{label.hint}</span>
      {failing.length > 0 && (
        <span className="ml-3 font-semibold text-live" data-testid="health-degraded">
          점검 필요: {failing.map(([name]) => name).join(', ')}
        </span>
      )}
    </div>
  )
}

export function ServerStatus() {
  const t = useTransport()
  const [m, setMetrics] = useState<Metrics | null>(null)
  const [health, setHealth] = useState<Health | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let live = true
    const read = () => {
      t.metrics()
        .then((x) => live && setMetrics(x))
        .catch((e) => live && setError(e instanceof Error ? e.message : '상태를 읽을 수 없습니다.'))
      t.health()
        .then((h) => live && setHealth(h))
        .catch(() => undefined)
    }
    read()
    const timer = setInterval(read, 5000)
    return () => {
      live = false
      clearInterval(timer)
    }
  }, [t])

  const used = m ? m.server.memory_total_bytes - m.server.memory_available_bytes : 0

  return (
    <div className="space-y-4">
      <Card title="서버">
        {error && (
          <p role="alert" className="mb-3 text-sm text-live">
            {error}
          </p>
        )}
        <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
          <Stat label="CPU" value={`${(m?.server.cpu_percent ?? 0).toFixed(0)}%`} />
          <Stat
            label="메모리"
            value={m ? `${formatBytes(used)} / ${formatBytes(m.server.memory_total_bytes)}` : '—'}
          />
          <Stat label="디스크 여유" value={m ? formatBytes(m.server.disk_available_bytes) : '—'} />
          <Stat label="누적 송출량" value={m ? formatBytes(m.server.egress_bytes) : '—'} />
        </div>
        {health && (
          <div className="mt-4 flex flex-wrap gap-2" data-testid="health-checks">
            {Object.entries(health.checks).map(([name, ok]) => (
              <Badge key={name} tone={ok ? 'ok' : 'live'}>
                {name} {ok ? 'OK' : 'FAIL'}
              </Badge>
            ))}
            <Badge>v{health.version}</Badge>
          </div>
        )}
      </Card>

      <Card title="방송별 지표">
        {!m || m.broadcasts.length === 0 ? (
          <p className="py-6 text-center text-sm text-ink-500">아직 방송이 없습니다.</p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-left text-xs">
              <thead className="text-ink-500">
                <tr>
                  <th className="py-2 pr-4 font-medium">방송</th>
                  <th className="py-2 pr-4 font-medium">상태</th>
                  <th className="py-2 pr-4 font-medium">송출 시간</th>
                  <th className="py-2 pr-4 font-medium">전송량</th>
                  <th className="py-2 pr-4 font-medium">평균 비트레이트</th>
                  <th className="py-2 pr-4 font-medium">재시작</th>
                  <th className="py-2 pr-4 font-medium">FFmpeg PID</th>
                  <th className="py-2 font-medium">마지막 오류</th>
                </tr>
              </thead>
              <tbody className="font-mono text-ink-300">
                {m.broadcasts.map((b) => (
                  <tr key={b.id} className="border-t border-ink-700" data-testid="metrics-row">
                    <td className="py-2 pr-4 font-sans text-ink-100">{b.name}</td>
                    <td className="py-2 pr-4">{RUNTIME_LABELS[b.runtime_state]}</td>
                    <td className="py-2 pr-4">{formatDurationKo(b.uptime_secs)}</td>
                    <td className="py-2 pr-4">{formatBytes(b.bytes_sent)}</td>
                    <td className="py-2 pr-4">
                      {(b.average_bitrate_bps / 1_000_000).toFixed(2)} Mbps
                    </td>
                    <td className="py-2 pr-4">{b.restart_count}</td>
                    <td className="py-2 pr-4">{b.ffmpeg_pid ?? '—'}</td>
                    <td className="py-2 font-sans text-live">{b.last_error ?? ''}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </div>
  )
}
