import { useCallback, useEffect, useState } from 'react'
import { Eye, FolderOpen, Trash2 } from 'lucide-react'
import { useAppStore } from '@/stores/useAppStore'
import { api, openUrl, revealPath } from '@/services/ipc'
import { encoderLabel } from '@/services/format'
import { Badge, Button, Card, Field, Input, Modal, Select, Toggle } from '@/components/ui'
import type { QuotaReport, StreamDiagnostics, YoutubeStatus } from '@/types'

/** Settings (§45), including the stream key panel. */
export function SettingsPage() {
  const { settings, refreshSettings, reportError, toast } = useAppStore()
  const [keyInput, setKeyInput] = useState('')
  const [revealed, setRevealed] = useState<string | null>(null)
  const [confirmReveal, setConfirmReveal] = useState(false)
  const [confirmClear, setConfirmClear] = useState(false)
  const [inUse, setInUse] = useState(0)
  const [restoreScheduler, setRestoreScheduler] = useState(true)

  useEffect(() => { void refreshSettings() }, [refreshSettings])
  useEffect(() => {
    api.schedulerStatus().then((v) => setRestoreScheduler(v.restore_on_launch)).catch(() => {})
  }, [])

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
        {/* §7: a machine that was watching the clock should be watching it
            again after a reboot — and one the user deliberately stopped must
            stay stopped. */}
        <Toggle
          label="예약 방송 상태 자동 복원"
          hint="재부팅 후 Louver Live가 실행되면, 이전에 [예약 방송 시작]을 눌러둔 상태를 그대로 복원합니다. 직접 중지하셨다면 복원하지 않습니다."
          checked={restoreScheduler}
          onChange={(v) => {
            setRestoreScheduler(v)
            api.schedulerSetRestore(v).catch(reportError)
          }}
        />
      </Card>

      <Card title="기본 송출">
        <p className="mb-3 text-xs text-ink-500">
          방송에 필요한 것은 스트림 키 하나뿐입니다. Google 로그인도, 개발자 설정도 필요하지 않습니다.
        </p>
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

      <YoutubeAccountCard />

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

      <Card title="고급">
        <dl className="space-y-2 text-sm">
          <Row label="FFmpeg" value={s.ffmpeg_version ?? '찾을 수 없음 (LL-CONFIG-002)'} />
          <Row label="FFmpeg 경로" value={s.ffmpeg_path ?? '—'} mono />
          {/*
            §7: the engine is chosen by probing, never by the user. This says
            which one was chosen, in words rather than in encoder names, and
            keeps the name itself for the people who came looking for it.
          */}
          <Row
            label="사용 중인 변환 엔진"
            value={
              <span className="inline-flex items-center gap-2">
                <span>{encoderLabel(s.hardware_encoder)}</span>
                <Badge tone={s.hardware_encoder === 'libx264' ? 'default' : 'ok'}>
                  {s.hardware_encoder === 'libx264' ? '소프트웨어' : '하드웨어 가속'}
                </Badge>
                <span className="font-mono text-xs text-ink-500">{s.hardware_encoder}</span>
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
            현재 플레이리스트에서 사용 중인 준비 파일 {inUse}개가 함께 삭제됩니다. 다시 방송하려면 영상을 한 번 더 준비해야 합니다.
          </p>
        ) : (
          <p>준비된 영상 캐시를 모두 삭제합니다. 원본 파일은 삭제되지 않습니다.</p>
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

/**
 * YouTube account (§1).
 *
 * The refresh token never appears here — only the channel it belongs to and
 * which store is holding it. Consent happens in the browser; this polls until
 * it lands, so the window is never blocked waiting on Google.
 */
function YoutubeAccountCard() {
  const { reportError, toast, settings } = useAppStore()
  const [yt, setYt] = useState<YoutubeStatus | null>(null)
  const [waiting, setWaiting] = useState(false)
  const [advanced, setAdvanced] = useState(false)
  const [manualUrl, setManualUrl] = useState<string | null>(null)
  const [clientId, setClientId] = useState('')
  const [clientSecret, setClientSecret] = useState('')
  const [quota, setQuota] = useState<QuotaReport | null>(null)

  const refresh = useCallback(
    () => api.youtubeStatus().then(setYt).catch(reportError),
    [reportError],
  )
  useEffect(() => { void refresh() }, [refresh])
  useEffect(() => { api.youtubeQuota().then(setQuota).catch(() => {}) }, [yt?.connected])

  // While consent is open in the browser there is nothing to do but watch for
  // it to finish.
  useEffect(() => {
    if (!waiting) return
    const t = setInterval(async () => {
      const s = await api.youtubeStatus().catch(() => null)
      if (!s) return
      setYt(s)
      if (s.connected) {
        setWaiting(false)
        toast({ kind: 'success', message: `${s.channel_title ?? 'YouTube'} 채널에 연결했습니다.` })
      } else if (s.connecting_error) {
        setWaiting(false)
        toast({ kind: 'error', message: s.connecting_error })
      }
    }, 2000)
    return () => clearInterval(t)
  }, [waiting, toast])

  if (!yt) return null

  /** Open Google's consent page and wait for the redirect to come back. */
  async function connect(switchAccount: boolean) {
    let url: string
    try {
      url = switchAccount ? await api.youtubeSwitchAccount() : await api.youtubeBeginConnect()
    } catch (e) {
      reportError(e)
      return
    }
    // The flow is already running and the loopback server is listening, so a
    // browser that will not open is not a dead end — show the address instead
    // of abandoning a consent attempt the user can still finish by hand.
    setWaiting(true)
    setManualUrl(null)
    try {
      await openUrl(url)
    } catch {
      setManualUrl(url)
    }
  }

  return (
    <Card title="YouTube 고급 기능 (선택)">
      {/* The whole point of this card is that it can be ignored. A person who
          only wants to broadcast has already finished above. */}
      <p className="mb-3 text-xs text-ink-500">
        연결하면 방송 제목·설명·태그를 앱에서 바꾸고, 자동 라이브 채팅을 쓸 수 있습니다.
        <span className="text-ink-400"> 선택 기능입니다. 방송만 사용하려면 연결할 필요가 없습니다.</span>
      </p>
      {yt.connected ? (
        <>
          <dl className="space-y-2">
            <Row label="채널" value={yt.channel_title ?? '—'} />
            <Row label="Channel ID" value={yt.channel_id ?? '—'} mono />
            <Row
              label="상태"
              value={<span className="inline-flex items-center gap-1.5 text-ok"><span className="h-1.5 w-1.5 rounded-full bg-ok" />연결됨</span>}
            />
            <Row label="토큰 저장 위치" value={yt.secret_backend} />
            {/* Free allowance only — there is no billing account behind this,
                so running out pauses the optional features until it resets and
                can never produce a charge. */}
            {quota && (
              <Row
                label="오늘 사용량"
                value={
                  <span className={quota.exhausted ? 'text-warn' : 'text-ink-300'}>
                    {quota.exhausted
                      ? '모두 사용함 · 내일 초기화'
                      : `${quota.used_percent}% · 무료 한도 내`}
                  </span>
                }
              />
            )}
          </dl>
          {!yt.secret_backend_is_secure && (
            <p className="mt-2 text-xs text-warn">
              이 컴퓨터에서는 키체인을 쓸 수 없어 토큰이 메모리에만 저장됩니다. 앱을 재시작하면 다시 연결해야 합니다.
            </p>
          )}
          <div className="mt-3 flex flex-wrap gap-2 border-t border-ink-700 pt-3">
            <Button onClick={() => void connect(true)} disabled={waiting}>
              {waiting ? '브라우저에서 진행 중…' : '계정 변경'}
            </Button>
            <Button
              onClick={async () => {
                try {
                  setYt(await api.youtubeDisconnect())
                  toast({ kind: 'info', message: 'YouTube 연결을 해제했습니다.' })
                } catch (e) { reportError(e) }
              }}
            >
              연결 해제
            </Button>
          </div>
        </>
      ) : (
        <>
          <dl className="space-y-2">
            <Row label="상태" value={<span className="text-ink-400">연결되지 않음</span>} />
          </dl>
          <div className="mt-3 border-t border-ink-700 pt-3">
            <Button onClick={() => void connect(false)} disabled={!yt.has_credentials || waiting}>
              {waiting ? '브라우저에서 진행 중…' : 'YouTube 계정 연결'}
            </Button>
          </div>
          {waiting && !manualUrl && (
            <p className="mt-2 text-xs text-ink-500">
              열린 브라우저 창에서 Google 로그인 후 권한을 허용해주세요. 완료되면 이 화면이 자동으로 바뀝니다.
            </p>
          )}
          {manualUrl && (
            <div className="mt-2 rounded border border-warn-dim bg-warn-dim/10 p-3">
              <p className="text-xs text-warn">
                브라우저를 열지 못했습니다. 아래 주소를 브라우저에 직접 붙여넣어 주세요.
                연결이 끝나면 이 화면이 자동으로 바뀝니다.
              </p>
              <p className="mt-2 break-all font-mono text-[11px] text-ink-300">{manualUrl}</p>
              <Button
                size="sm"
                className="mt-2"
                onClick={() => navigator.clipboard?.writeText(manualUrl).then(
                  () => toast({ kind: 'success', message: '주소를 복사했습니다.' }),
                  () => toast({ kind: 'error', message: '복사하지 못했습니다. 주소를 직접 선택해주세요.' }),
                )}
              >
                주소 복사
              </Button>
            </div>
          )}
          {!yt.has_credentials && (
            <p className="mt-2 text-xs text-warn">
              이 빌드에는 Louver Live의 YouTube 클라이언트가 포함되어 있지 않습니다.
              정식 릴리스 빌드에서는 버튼 하나로 연결됩니다.
            </p>
          )}
          {yt.connecting_error && <p className="mt-2 text-xs text-live">{yt.connecting_error}</p>}
          {yt.last_auth_diagnostic && (
            <div className="mt-2 rounded border border-ink-700 p-2">
              <p className="text-[11px] uppercase tracking-wider text-ink-500">
                마지막 토큰 응답
              </p>
              {/* Google's own words, unsummarised: this is what decides whether
                  a client secret is needed at all. */}
              <p className="mt-1 break-all font-mono text-[11px] text-ink-300">
                {yt.last_auth_diagnostic}
              </p>
            </div>
          )}
        </>
      )}

      {/* §7: a custom OAuth client is a developer tool, not a product feature.
          It appears only with 개발자 모드 on, and only behind a disclosure. */}
      {settings?.developer_mode && (
        <div className="mt-4 border-t border-ink-800 pt-3">
          <button
            type="button"
            className="text-[11px] uppercase tracking-wider text-ink-500 hover:text-ink-300"
            onClick={() => setAdvanced((v) => !v)}
          >
            고급: 자체 OAuth 클라이언트 {advanced ? '숨기기' : '보기'}
          </button>
          {advanced && (
            <div className="mt-2 space-y-2">
              <p className="text-xs text-ink-500">
                비워두면 빌드에 포함된 Louver Live 클라이언트를 씁니다.
                {yt.using_custom_client && ' 현재 자체 클라이언트를 사용 중입니다.'}
              </p>
              <Input
                value={clientId}
                aria-label="OAuth 클라이언트 ID"
                placeholder={yt.client_id_hint ?? '000000-xxxx.apps.googleusercontent.com'}
                onChange={(e) => setClientId(e.target.value)}
              />
              <Input
                value={clientSecret}
                type="password"
                aria-label="OAuth 클라이언트 보안 비밀"
                placeholder="클라이언트 보안 비밀"
                onChange={(e) => setClientSecret(e.target.value)}
              />
              {/* Not a switch any more. Google refuses this desktop client's
                  token exchange without a client_secret — "invalid_request:
                  client_secret is missing" — so it is always sent when the
                  build has one. All that is left to show is whether it does,
                  never what it is. */}
              <p className="text-xs text-ink-500">
                토큰 교환에는 client_secret이 항상 함께 전송됩니다.
                이 빌드의 보안 비밀: <span className={yt.has_client_secret ? 'text-ok' : 'text-warn'}>
                  {yt.has_client_secret ? 'configured' : 'missing'}
                </span>
              </p>
              <Button
                size="sm"
                onClick={async () => {
                  try {
                    setYt(await api.youtubeSetCredentials(clientId, clientSecret))
                    setClientSecret('')
                    toast({ kind: 'success', message: 'OAuth 클라이언트를 저장했습니다.' })
                  } catch (e) { reportError(e) }
                }}
              >
                저장
              </Button>
            </div>
          )}
        </div>
      )}
    </Card>
  )
}
