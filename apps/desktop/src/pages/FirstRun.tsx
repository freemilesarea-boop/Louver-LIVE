import { useState } from 'react'
import { Check, ChevronRight } from 'lucide-react'
import { api } from '@/services/ipc'
import { useAppStore } from '@/stores/useAppStore'
import { Button, Field, Input, Select, Toggle } from '@/components/ui'

/** First-run wizard (§44). Every step after the intro can be skipped. */
export function FirstRun({ onDone }: { onDone: () => void }) {
  const { settings, reportError, refreshSettings } = useAppStore()
  const [step, setStep] = useState(0)
  const [key, setKey] = useState('')
  const [profile, setProfile] = useState(settings?.output_profile ?? '1080p30')
  const [autostart, setAutostart] = useState(false)
  const [busy, setBusy] = useState(false)

  const steps = ['소개', '스트림 키', '송출 품질', '자동 시작', '완료']

  async function finish() {
    setBusy(true)
    try {
      if (key.trim()) await api.setStreamKey(key.trim())
      await api.setSetting('output_profile', profile)
      await api.setSetting('launch_at_startup', String(autostart))
      await api.setSetting('first_run_complete', 'true')
      await refreshSettings()
      onDone()
    } catch (e) {
      reportError(e)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-ink-950 p-6">
      <div className="w-full max-w-lg rounded-xl border border-ink-700 bg-ink-850 p-7">
        <div className="mb-6 flex items-center gap-2">
          {steps.map((s, i) => (
            <div key={s} className="flex items-center gap-2">
              <span
                className={`flex h-6 w-6 items-center justify-center rounded-full text-[11px] ${
                  i < step ? 'bg-ok text-ink-950' : i === step ? 'bg-ink-100 text-ink-950' : 'bg-ink-700 text-ink-500'
                }`}
              >
                {i < step ? <Check size={12} /> : i + 1}
              </span>
              {i < steps.length - 1 && <span className="h-px w-4 bg-ink-700" />}
            </div>
          ))}
        </div>

        {step === 0 && (
          <div className="space-y-3">
            <h1 className="text-xl font-bold tracking-[0.2em] text-ink-100">LOUVER LIVE</h1>
            <p className="text-sm leading-relaxed text-ink-300">
              여러 개의 음악 영상을 넣어두면, 지정한 시간 동안 자동으로 순차 반복하며 YouTube Live로 송출합니다.
            </p>
            <ol className="space-y-1.5 pt-2 text-sm text-ink-400">
              <li>1. 영상 넣기</li>
              <li>2. 순서 정하기</li>
              <li>3. 방송 시간 정하기</li>
              <li>4. 방송 시작</li>
            </ol>
          </div>
        )}

        {step === 1 && (
          <div>
            <h2 className="mb-1 text-base text-ink-100">YouTube 스트림 키</h2>
            <p className="mb-4 text-xs text-ink-500">
              YouTube Studio → 실시간 스트리밍에서 확인할 수 있습니다. 지금 건너뛰고 나중에 설정에서 입력해도 됩니다.
            </p>
            <Field label="스트림 키">
              <Input
                type="password"
                aria-label="스트림 키"
                placeholder="xxxx-xxxx-xxxx-xxxx-xxxx"
                value={key}
                onChange={(e) => setKey(e.target.value)}
              />
            </Field>
          </div>
        )}

        {step === 2 && (
          <div>
            <h2 className="mb-1 text-base text-ink-100">기본 송출 품질</h2>
            <p className="mb-4 text-xs text-ink-500">업로드 속도가 넉넉하지 않다면 720p30을 선택하세요.</p>
            <Field label="송출 품질">
              <Select aria-label="송출 품질" value={profile} onChange={(e) => setProfile(e.target.value)}>
                <option value="1080p30">1080p30 (권장) · 10 Mbps</option>
                <option value="720p30">720p30 (저대역폭) · 4 Mbps</option>
              </Select>
            </Field>
          </div>
        )}

        {step === 3 && (
          <div>
            <h2 className="mb-1 text-base text-ink-100">컴퓨터 자동 시작</h2>
            <p className="mb-2 text-xs text-ink-500">
              켜두면 컴퓨터가 재부팅되어도 예약된 방송이 자동으로 복구됩니다.
            </p>
            <Toggle
              label="컴퓨터가 켜지면 Louver Live 자동 실행"
              checked={autostart}
              onChange={setAutostart}
            />
          </div>
        )}

        {step === 4 && (
          <div className="space-y-3">
            <h2 className="text-base text-ink-100">준비가 끝났습니다</h2>
            <p className="text-sm text-ink-400">
              플레이리스트에 영상을 추가하고 방송 시간을 정하면, 나머지는 Louver Live가 알아서 처리합니다.
            </p>
            <p className="rounded border border-ink-700 p-3 text-xs leading-relaxed text-ink-500">
              방송 중에는 컴퓨터와 인터넷 연결이 유지되어야 합니다. 노트북의 경우 덮개를 닫으면 방송이 중단될 수 있습니다.
            </p>
          </div>
        )}

        <div className="mt-7 flex items-center justify-between">
          <button
            onClick={() => void finish()}
            className="text-xs text-ink-500 hover:text-ink-300"
            disabled={busy}
          >
            건너뛰기
          </button>
          {step < steps.length - 1 ? (
            <Button variant="primary" onClick={() => setStep((s) => s + 1)}>
              <span className="inline-flex items-center gap-1.5">다음 <ChevronRight size={14} /></span>
            </Button>
          ) : (
            <Button variant="live" onClick={() => void finish()} disabled={busy}>시작하기</Button>
          )}
        </div>
      </div>
    </div>
  )
}
