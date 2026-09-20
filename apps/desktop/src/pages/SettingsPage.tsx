import { useEffect, useState } from 'react'
import { Eye, FolderOpen, ShieldAlert, ShieldCheck, Trash2 } from 'lucide-react'
import { useAppStore } from '@/stores/useAppStore'
import { api, pickLicenseFile, revealPath } from '@/services/ipc'
import { Badge, Button, Card, Field, Input, Modal, Select, Toggle } from '@/components/ui'
import type { StreamDiagnostics } from '@/types'

/** Settings (§45), including the stream key and licence panels. */
export function SettingsPage() {
  const { settings, refreshSettings, reportError, toast } = useAppStore()
  const [keyInput, setKeyInput] = useState('')
  const [revealed, setRevealed] = useState<string | null>(null)
  const [confirmReveal, setConfirmReveal] = useState(false)
  const [confirmClear, setConfirmClear] = useState(false)
  const [inUse, setInUse] = useState(0)

  useEffect(() => { void refreshSettings() }, [refreshSettings])

  if (!settings) return <p className="text-sm text-ink-500">설정을 불러오는 중…</p>

  const s = settings
  const set = async (key: string, value: string) => {
    try {
      await api.setSetting(key, value)
      await refreshSettings()
    } catch (e) { reportError(e) }
  }
  const setFlag = (key: string) => (v: boolean) => void set(key, String(v))

  return (
    <div className="space-y-4">
      <h1 className="text-lg font-semibold text-ink-100">설정</h1>

      <Card title="일반">
        <Toggle
          label="컴퓨터가 켜지면 Louver Live 자동 실행"
          hint="예약된 방송을 놓치지 않으려면 켜두세요."
          checked={s.launch_at_startup}
          onChange={setFlag('launch_at_startup')}
        />
        <Toggle label="최소화된 상태로 시작" checked={s.start_minimized} onChange={setFlag('start_minimized')} />
        <Toggle
          label="창을 닫으면 트레이로 최소화"
          hint="방송 중에는 항상 트레이로 이동하며, 프로그램은 계속 실행됩니다."
          checked={s.minimize_to_tray}
          onChange={setFlag('minimize_to_tray')}
        />
      </Card>

      <Card title="송출">
        <Field label="RTMPS 서버 주소">
          <Input
            aria-label="RTMPS 서버 주소"
            defaultValue={s.rtmps_url}
            onBlur={(e) => void set('rtmps_url', e.target.value)}
          />
        </Field>

        <Field
          label="YouTube 스트림 키"
          hint={
            s.secret_backend_is_secure
              ? `${s.secret_backend}에 안전하게 저장됩니다. 로그에는 기록되지 않습니다.`
              : `주의: 이 컴퓨터에서는 ${s.secret_backend}만 사용할 수 있어 재시작 시 키가 사라집니다.`
          }
        >
          <div className="flex gap-2">
            <Input
              type="password"
              aria-label="YouTube 스트림 키"
              placeholder={s.has_stream_key ? s.stream_key_masked : 'xxxx-xxxx-xxxx-xxxx-xxxx'}
              value={keyInput}
              onChange={(e) => setKeyInput(e.target.value)}
            />
            <Button
              variant="primary"
              disabled={!keyInput.trim()}
              onClick={async () => {
                try {
                  await api.setStreamKey(keyInput.trim())
                  setKeyInput('')
                  toast({ kind: 'success', message: '스트림 키를 저장했습니다.' })
                  await refreshSettings()
                } catch (e) { reportError(e) }
              }}
            >
              저장
            </Button>
            {s.has_stream_key && (
              <>
                <Button aria-label="스트림 키 보기" onClick={() => setConfirmReveal(true)}><Eye size={15} /></Button>
                <Button
                  aria-label="스트림 키 삭제"
                  onClick={async () => {
                    try {
                      await api.clearStreamKey()
                      await refreshSettings()
                    } catch (e) { reportError(e) }
                  }}
                >
                  <Trash2 size={15} />
                </Button>
              </>
            )}
          </div>
          {s.has_stream_key && s.stream_key_hint && (
            <p className="mt-1.5 font-mono text-[11px] text-ink-500">저장됨 · {s.stream_key_hint}</p>
          )}
        </Field>

        <Field label="송출 품질">
          <Select
            aria-label="송출 품질"
            value={s.output_profile}
            onChange={(e) => void set('output_profile', e.target.value)}
          >
            {s.profiles.map((p) => (
              <option key={p.id} value={p.id}>{p.label} · {(p.video_kbps / 1000).toFixed(0)} Mbps</option>
            ))}
          </Select>
        </Field>

        <Field
          label="송출 모드"
          hint="Stream Copy는 방송 중 영상을 재인코딩하지 않아 CPU 사용량이 낮습니다. 호환 모드는 문제가 있을 때만 사용하세요."
        >
          <Select aria-label="송출 모드" value={s.stream_mode} onChange={(e) => void set('stream_mode', e.target.value)}>
            <option value="stream_copy">Stream Copy (권장)</option>
            <option value="compatibility_encode">호환 모드 (실시간 인코딩)</option>
          </Select>
        </Field>

        <Toggle
          label="연결이 끊기면 자동으로 다시 연결"
          checked={s.auto_reconnect}
          onChange={setFlag('auto_reconnect')}
        />
      </Card>

      <Card title="저장공간">
        <Field label="캐시 위치"><Input readOnly value={s.cache_location} aria-label="캐시 위치" /></Field>
        <div className="flex items-center justify-between py-2">
          <span className="text-sm text-ink-300">캐시 크기 <span className="ml-2 font-mono text-ink-100">{s.cache_size_label}</span></span>
          <Button
            onClick={async () => {
              try {
                setInUse(await api.cacheInUseCount())
                setConfirmClear(true)
              } catch (e) { reportError(e) }
            }}
          >
            캐시 전체 삭제
          </Button>
        </div>
      </Card>

      <Card title="라이선스">
        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              {s.license.status === 'valid' || s.license.status === 'development' ? (
                <ShieldCheck size={16} className="text-ok" />
              ) : (
                <ShieldAlert size={16} className="text-live" />
              )}
              <span className="text-sm text-ink-100">{s.license.message}</span>
            </div>
            {s.license.status !== 'valid' && s.license.status !== 'development' && (
              <p className="mt-1.5 text-xs text-ink-500">
                라이선스가 없어도 앱은 사용할 수 있지만, YouTube 송출은 시작할 수 없습니다.
              </p>
            )}
          </div>
          <Button
            onClick={async () => {
              const p = await pickLicenseFile()
              if (!p) return
              try {
                await api.installLicense(p)
                toast({ kind: 'success', message: '라이선스를 등록했습니다.' })
                await refreshSettings()
              } catch (e) { reportError(e) }
            }}
          >
            라이선스 파일 등록
          </Button>
        </div>
      </Card>

      <Card title="고급">
        <dl className="space-y-2 text-sm">
          <Row label="FFmpeg" value={s.ffmpeg_version ?? '찾을 수 없음 (LL-CONFIG-002)'} />
          <Row label="FFmpeg 경로" value={s.ffmpeg_path ?? '—'} mono />
          <Row
            label="최적화 인코더"
            value={
              <span className="inline-flex items-center gap-2">
                <span className="font-mono">{s.hardware_encoder}</span>
                <Badge tone={s.hardware_encoder === 'libx264' ? 'default' : 'ok'}>
                  {s.hardware_encoder === 'libx264' ? '소프트웨어' : '하드웨어 가속'}
                </Badge>
              </span>
            }
          />
          <Row label="버전" value={s.app_version} mono />
        </dl>
        <div className="mt-3 flex items-center justify-between border-t border-ink-700 pt-3">
          <span className="text-sm text-ink-300">로그 폴더</span>
          <Button size="sm" onClick={() => void revealPath(s.logs_dir).catch(reportError)}>
            <span className="inline-flex items-center gap-2"><FolderOpen size={14} /> 열기</span>
          </Button>
        </div>
        <Toggle
          label="개발자 모드"
          hint="크래시 시뮬레이션 등 QA 도구를 표시합니다."
          checked={s.developer_mode}
          onChange={setFlag('developer_mode')}
        />
        {s.developer_mode && (
          <div className="mt-2 space-y-3 rounded border border-ink-700 p-3">
            <StreamDiagnosticsPanel />
            <div className="flex flex-wrap items-center gap-2 border-t border-ink-800 pt-3">
              <Button
                size="sm"
                onClick={async () => {
                  try {
                    await api.simulateCrash()
                    toast({ kind: 'info', message: 'FFmpeg를 강제 종료했습니다. 자동 복구를 확인하세요.' })
                  } catch (e) { reportError(e) }
                }}
              >
                FFmpeg 크래시 시뮬레이션
              </Button>
              <Toggle
                label="장치 바인딩 강제"
                checked={s.enforce_device_binding}
                onChange={setFlag('enforce_device_binding')}
              />
            </div>
          </div>
        )}
      </Card>

      <Modal
        open={confirmReveal}
        title="스트림 키를 표시할까요?"
        onClose={() => { setConfirmReveal(false); setRevealed(null) }}
        footer={
          <>
            <Button onClick={() => { setConfirmReveal(false); setRevealed(null) }}>닫기</Button>
            {!revealed && (
              <Button
                variant="danger"
                onClick={async () => {
                  try {
                    setRevealed(await api.revealStreamKey())
                  } catch (e) { reportError(e) }
                }}
              >
                표시
              </Button>
            )}
          </>
        }
      >
        {revealed ? (
          <code className="block select-all break-all rounded bg-ink-900 p-3 font-mono text-ink-100">{revealed}</code>
        ) : (
          <p>주변에 화면을 볼 수 있는 사람이 없는지 확인하세요. 스트림 키가 노출되면 다른 사람이 채널로 방송할 수 있습니다.</p>
        )}
      </Modal>

      <Modal
        open={confirmClear}
        title="캐시를 모두 삭제할까요?"
        onClose={() => setConfirmClear(false)}
        footer={
          <>
            <Button onClick={() => setConfirmClear(false)}>취소</Button>
            <Button
              variant="danger"
              onClick={async () => {
                setConfirmClear(false)
                try {
                  const freed = await api.clearCache()
                  toast({ kind: 'success', message: `${freed}를 확보했습니다.` })
                  await refreshSettings()
                } catch (e) { reportError(e) }
              }}
            >
              삭제
            </Button>
          </>
        }
      >
        {inUse > 0 ? (
          <p className="text-warn">
            현재 플레이리스트에서 사용 중인 최적화 파일 {inUse}개가 함께 삭제됩니다. 다시 방송하려면 최적화를 한 번 더 실행해야 합니다.
          </p>
        ) : (
          <p>최적화된 영상 캐시를 모두 삭제합니다. 원본 파일은 삭제되지 않습니다.</p>
        )}
      </Modal>
    </div>
  )
}

/**
 * Shows what the live FFmpeg process is actually doing (§13).
 *
 * The dashboard badge reflects configuration. This reflects the real argv, so
 * "is this session actually stream copy?" can be answered from evidence — the
 * first thing to check when CPU is higher than expected.
 */
function StreamDiagnosticsPanel() {
  const { reportError } = useAppStore()
  const [d, setD] = useState<StreamDiagnostics | null>(null)
  const [showCommand, setShowCommand] = useState(false)

  useEffect(() => {
    const load = () => api.streamDiagnostics().then(setD).catch(reportError)
    void load()
    const t = setInterval(load, 3000)
    return () => clearInterval(t)
  }, [reportError])

  if (!d) return <p className="text-xs text-ink-500">진단 정보를 불러오는 중…</p>

  const ok = d.argv_is_stream_copy && !d.mismatch
  return (
    <div>
      <div className="mb-2 flex flex-wrap items-center gap-2">
        <span className="text-xs uppercase tracking-wider text-ink-500">Streaming Mode</span>
        <Badge tone={d.masked_command.length === 0 ? 'default' : ok ? 'ok' : 'warn'}>
          {d.masked_command.length === 0
            ? '—'
            : d.argv_is_stream_copy
              ? 'STREAM COPY'
              : 'COMPATIBILITY MODE'}
        </Badge>
        {d.ffmpeg_pid != null && (
          <span className="font-mono text-[11px] text-ink-500">
            pid {d.ffmpeg_pid} · CPU {d.ffmpeg_cpu_percent.toFixed(1)}%
          </span>
        )}
      </div>

      <p className={`text-xs ${d.mismatch ? 'text-live' : 'text-ink-400'}`}>{d.verdict}</p>

      {/* What the supervisor is actually seeing (§11). Every value here is read
          from the live process, not from the UI's own idea of the state. */}
      <dl className="mt-3 grid grid-cols-2 gap-x-6 gap-y-1.5 font-mono text-[11px] sm:grid-cols-3">
        <Diag label="State" value={d.state} />
        <Diag label="FFmpeg PID" value={d.ffmpeg_pid != null ? String(d.ffmpeg_pid) : '—'} />
        <Diag label="Reconnect" value={String(d.reconnect_count)} tone={d.reconnect_count > 0 ? 'warn' : undefined} />
        <Diag label="CPU" value={`${d.ffmpeg_cpu_percent.toFixed(1)}%`} />
        <Diag
          label="RAM"
          value={d.ffmpeg_memory_bytes ? `${(d.ffmpeg_memory_bytes / 1048576).toFixed(0)} MB` : '—'}
        />
        <Diag
          label="Last progress"
          value={d.seconds_since_progress != null ? `${d.seconds_since_progress}s ago` : '—'}
          tone={(d.seconds_since_progress ?? 0) > 15 ? 'warn' : undefined}
        />
        <Diag label="RTMP" value={d.publishing ? 'CONNECTED' : '—'} />
        <Diag
          label="Sent"
          value={d.bytes_sent ? `${(d.bytes_sent / 1e9).toFixed(2)} GB` : '—'}
        />
        {d.local_test_sink.kind !== 'None' && (
          <Diag
            label="Test sink"
            value={d.local_test_sink.kind === 'Rtmp' ? '로컬 RTMP' : '파일'}
            tone={d.local_test_sink.kind === 'Rtmp' ? undefined : 'warn'}
          />
        )}
      </dl>
      {d.local_test_sink.kind !== 'None' && (
        <p className="mt-1 break-all font-mono text-[10px] text-ink-500">{d.local_test_sink.target}</p>
      )}

      {d.video_encoder_args.length > 0 && (
        <p className="mt-1 font-mono text-[11px] text-warn">
          영상 인코더 인자: {d.video_encoder_args.join(' ')}
        </p>
      )}

      {d.masked_command.length > 0 && (
        <>
          <button
            onClick={() => setShowCommand((v) => !v)}
            className="mt-2 text-[11px] text-ink-400 underline-offset-2 hover:underline"
          >
            {showCommand ? '실행 중인 명령 숨기기' : '실행 중인 명령 보기'}
          </button>
          {showCommand && (
            <pre
              data-selectable
              className="mt-1.5 max-h-40 overflow-auto whitespace-pre-wrap break-all rounded bg-ink-950 p-2 font-mono text-[10px] text-ink-500"
            >
              {d.masked_command.join(' ')}
            </pre>
          )}
        </>
      )}
    </div>
  )
}

function Row({ label, value, mono }: { label: string; value: React.ReactNode; mono?: boolean }) {
  return (
    <div className="flex items-start justify-between gap-4">
      <dt className="shrink-0 text-ink-400">{label}</dt>
      <dd className={`min-w-0 break-all text-right text-ink-200 ${mono ? 'font-mono text-xs' : ''}`}>{value}</dd>
    </div>
  )
}

/** One row of the supervisor readout. */
function Diag({ label, value, tone }: { label: string; value: string; tone?: 'warn' }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <dt className="text-ink-500">{label}</dt>
      <dd className={tone === 'warn' ? 'text-warn' : 'text-ink-200'}>{value}</dd>
    </div>
  )
}
