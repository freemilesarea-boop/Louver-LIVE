import { useEffect, useState } from 'react'
import { AlertCircle, CheckCircle2, Info, X } from 'lucide-react'
import { useAppStore } from '@/stores/useAppStore'
import { listen } from '@/services/ipc'
import { Sidebar } from '@/components/Sidebar'
import { Dashboard } from '@/pages/Dashboard'
import { PlaylistPage } from '@/pages/PlaylistPage'
import { SchedulePage } from '@/pages/SchedulePage'
import { LogsPage } from '@/pages/LogsPage'
import { SettingsPage } from '@/pages/SettingsPage'
import { FirstRun } from '@/pages/FirstRun'
import { Button, Modal } from '@/components/ui'

export function App() {
  const { page, refreshAll, subscribe, settings, booted, startupNotice, dismissNotice } = useAppStore()
  const [wizardDone, setWizardDone] = useState(false)
  const [closeWhileLive, setCloseWhileLive] = useState(false)

  useEffect(() => {
    void refreshAll()
    let off: (() => void) | undefined
    void subscribe().then((f) => { off = f })
    return () => off?.()
  }, [refreshAll, subscribe])

  // §22: the shell hides the window and tells the UI, which explains why.
  useEffect(() => {
    let off: (() => void) | undefined
    void listen<void>('louver://close-while-live', () => setCloseWhileLive(true)).then((f) => { off = f })
    return () => off?.()
  }, [])

  if (!booted) {
    return <div className="flex min-h-screen items-center justify-center text-sm text-ink-500">불러오는 중…</div>
  }

  if (settings && !settings.first_run_complete && !wizardDone) {
    return <FirstRun onDone={() => setWizardDone(true)} />
  }

  return (
    <div className="flex min-h-screen bg-ink-950">
      <Sidebar />
      <main className="min-w-0 flex-1 overflow-y-auto p-6">
        {page === 'dashboard' && <Dashboard />}
        {page === 'playlist' && <PlaylistPage />}
        {page === 'schedule' && <SchedulePage />}
        {page === 'logs' && <LogsPage />}
        {page === 'settings' && <SettingsPage />}
      </main>

      <Toasts />

      <Modal
        open={startupNotice != null}
        title="이전 실행 알림"
        onClose={dismissNotice}
        footer={<Button variant="primary" onClick={dismissNotice}>확인</Button>}
      >
        {startupNotice}
      </Modal>

      <Modal
        open={closeWhileLive}
        title="현재 방송 중입니다"
        onClose={() => setCloseWhileLive(false)}
        footer={<Button variant="primary" onClick={() => setCloseWhileLive(false)}>확인</Button>}
      >
        창을 숨겼습니다. 방송은 계속 진행됩니다. 완전히 종료하려면 트레이 아이콘에서 &quot;프로그램 종료&quot;를 선택하세요.
      </Modal>
    </div>
  )
}

function Toasts() {
  const { toasts, dismissToast } = useAppStore()
  const [expanded, setExpanded] = useState<number | null>(null)
  if (!toasts.length) return null

  return (
    <div className="pointer-events-none fixed bottom-4 right-4 z-40 flex w-96 flex-col gap-2">
      {toasts.map((t) => {
        const Icon = t.kind === 'error' ? AlertCircle : t.kind === 'success' ? CheckCircle2 : Info
        const tone =
          t.kind === 'error' ? 'border-live-dim bg-ink-850' :
          t.kind === 'success' ? 'border-ok-dim bg-ink-850' : 'border-ink-700 bg-ink-850'
        return (
          <div key={t.id} role="alert" className={`pointer-events-auto rounded-lg border p-3 shadow-xl ${tone}`}>
            <div className="flex items-start gap-2.5">
              <Icon
                size={16}
                className={`mt-0.5 shrink-0 ${t.kind === 'error' ? 'text-live' : t.kind === 'success' ? 'text-ok' : 'text-ink-400'}`}
              />
              <div className="min-w-0 flex-1">
                <p className="text-sm text-ink-100">{t.message}</p>
                {t.code && <p className="mt-0.5 font-mono text-[10px] text-ink-600">{t.code}</p>}
                {/* §35: technical detail stays behind a disclosure. */}
                {t.detail && (
                  <>
                    <button
                      onClick={() => setExpanded(expanded === t.id ? null : t.id)}
                      className="mt-1 text-[11px] text-ink-400 underline-offset-2 hover:underline"
                    >
                      {expanded === t.id ? '상세정보 접기' : '상세정보'}
                    </button>
                    {expanded === t.id && (
                      <pre className="mt-1.5 max-h-32 overflow-auto whitespace-pre-wrap break-all rounded bg-ink-950 p-2 font-mono text-[10px] text-ink-500">
                        {t.detail}
                      </pre>
                    )}
                  </>
                )}
              </div>
              <button onClick={() => dismissToast(t.id)} aria-label="알림 닫기" className="text-ink-600 hover:text-ink-300">
                <X size={14} />
              </button>
            </div>
          </div>
        )
      })}
    </div>
  )
}
