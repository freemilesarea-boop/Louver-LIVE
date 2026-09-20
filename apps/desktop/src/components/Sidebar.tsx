import { CalendarClock, LayoutDashboard, ListVideo, Radio, ScrollText, Settings2 } from 'lucide-react'
import { useAppStore, type Page } from '@/stores/useAppStore'
import { StatusPill } from './StatusPill'

const NAV: { id: Page; label: string; icon: typeof LayoutDashboard }[] = [
  { id: 'dashboard', label: '대시보드', icon: LayoutDashboard },
  { id: 'playlist', label: '플레이리스트', icon: ListVideo },
  { id: 'schedule', label: '방송 예약', icon: CalendarClock },
  { id: 'broadcast', label: '방송 설정', icon: Radio },
  { id: 'logs', label: '로그', icon: ScrollText },
  { id: 'settings', label: '설정', icon: Settings2 },
]

export function Sidebar() {
  const { page, setPage, status } = useAppStore()
  return (
    <nav className="flex w-56 shrink-0 flex-col border-r border-ink-800 bg-ink-900" aria-label="주 메뉴">
      <div className="border-b border-ink-800 px-5 py-5">
        <h1 className="text-sm font-bold tracking-[0.2em] text-ink-100">LOUVER LIVE</h1>
        <div className="mt-2">
          <StatusPill state={status?.supervisor.state ?? 'IDLE'} dryRun={status?.dry_run} />
        </div>
      </div>
      <ul className="flex-1 p-2">
        {NAV.map(({ id, label, icon: Icon }) => (
          <li key={id}>
            <button
              onClick={() => setPage(id)}
              aria-current={page === id ? 'page' : undefined}
              className={`flex w-full items-center gap-3 rounded-md px-3 py-2 text-sm transition-colors ${
                page === id ? 'bg-ink-800 text-ink-100' : 'text-ink-400 hover:bg-ink-850 hover:text-ink-200'
              }`}
            >
              <Icon size={16} />
              {label}
            </button>
          </li>
        ))}
      </ul>
      <p className="px-5 py-4 text-[11px] leading-relaxed text-ink-600">
        방송 중에는 컴퓨터가 켜져 있어야 합니다.
      </p>
    </nav>
  )
}
