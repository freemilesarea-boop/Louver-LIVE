/**
 * The web app: sign in, then three screens against one account.
 *
 * There is no environment check anywhere below here. The transport was chosen in
 * `main.tsx`, and this file only knows that it has one.
 */
import { useEffect, useState } from 'react'
import { Button } from '@/components/ui'
import { CloudDashboard } from './pages/CloudDashboard'
import { Destinations } from './pages/Destinations'
import { MediaLibrary } from './pages/MediaLibrary'
import { DeploymentBanner, ServerStatus } from './pages/ServerStatus'
import { SignIn } from './pages/SignIn'
import { useTransport } from './TransportContext'
import type { Me } from './cloud'

type Tab = 'broadcasts' | 'media' | 'destinations' | 'status'

const TABS: { id: Tab; label: string }[] = [
  { id: 'broadcasts', label: '방송' },
  { id: 'media', label: '영상' },
  { id: 'destinations', label: '송출 대상' },
  { id: 'status', label: '서버 상태' },
]

export function App() {
  const t = useTransport()
  const [me, setMe] = useState<Me | null>(null)
  const [checked, setChecked] = useState(false)
  const [tab, setTab] = useState<Tab>('broadcasts')

  useEffect(() => {
    let live = true
    // The cookie may already be valid from a previous visit, so ask before
    // showing a sign-in form.
    t.me()
      .then((m) => live && setMe(m))
      .catch(() => undefined)
      .finally(() => live && setChecked(true))
    return () => {
      live = false
    }
  }, [t])

  if (!checked) {
    return <div className="flex min-h-screen items-center justify-center text-sm text-ink-500">불러오는 중…</div>
  }

  if (!me) {
    return (
      <>
        <DeploymentBanner />
        <SignIn onSignedIn={setMe} />
      </>
    )
  }

  return (
    <div className="min-h-screen bg-ink-950 text-ink-100">
      <DeploymentBanner />
      <header className="flex items-center justify-between border-b border-ink-700 px-6 py-3">
        <div className="flex items-center gap-6">
          <span className="text-sm font-semibold">Louver Live</span>
          <nav className="flex gap-1">
            {TABS.map((x) => (
              <button
                key={x.id}
                onClick={() => setTab(x.id)}
                aria-current={tab === x.id ? 'page' : undefined}
                className={`rounded-md px-3 py-1.5 text-sm ${
                  tab === x.id ? 'bg-ink-800 text-ink-100' : 'text-ink-400 hover:text-ink-100'
                }`}
              >
                {x.label}
              </button>
            ))}
          </nav>
        </div>
        <div className="flex items-center gap-3">
          <span className="text-xs text-ink-500">{me.email}</span>
          <Button
            size="sm"
            onClick={async () => {
              await t.logout()
              setMe(null)
            }}
          >
            로그아웃
          </Button>
        </div>
      </header>
      <main className="mx-auto max-w-4xl p-6">
        {tab === 'broadcasts' && <CloudDashboard />}
        {tab === 'media' && <MediaLibrary />}
        {tab === 'destinations' && <Destinations />}
        {tab === 'status' && <ServerStatus />}
      </main>
    </div>
  )
}
